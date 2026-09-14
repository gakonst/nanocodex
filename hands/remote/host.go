package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/coder/websocket"
	"github.com/pion/webrtc/v4"
)

type hostConfig struct {
	Origin, CredentialFile, MachineID, Name, Waymote string
	IncludeLoopback                                  bool
	Frames                                           bool
	Width, Height                                    int
	ICERenewalInterval                               time.Duration
	quiet                                            bool
	published                                        func()
	capture                                          *waymoteCapture
}
type hostEvent struct {
	viewer         string
	input          []byte
	motion         bool
	closed         bool
	message        *remoteMessage
	created        time.Time
	prepared       bool
	ice            []webrtc.ICEServer
	err            error
	refresh        bool
	localCandidate *remoteSignal
	agentRequest   string
	agentResult    *agentResult
	frame          *agentResult
}
type hostPeer struct {
	viewerID       string
	frames         bool
	framePending   bool
	connection     *webrtc.PeerConnection
	control        *webrtc.DataChannel
	candidates     []webrtc.ICECandidateInit
	answered       bool
	renewAt        time.Time
	answerDeadline time.Time
	renewal        context.CancelFunc
}
type controlMessage struct {
	Type       string `json:"type"`
	Generation string `json:"generation,omitempty"`
}

// All peer, lease and input state changes are serialized by the host loop.
// Motion has a replaceable slot per viewer; reliable input cannot queue behind
// an old stream of mouse movements.
func serveWayland(parent context.Context, config hostConfig) error {
	if config.ICERenewalInterval <= 0 {
		config.ICERenewalInterval = 20 * time.Minute
	}
	if !regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$`).MatchString(config.MachineID) || config.Name == "" || len(config.Name) > 128 || config.Width < 1 || config.Height < 1 || config.Width > 16384 || config.Height > 16384 {
		return errors.New("invalid remote Hand metadata")
	}
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	service, err := newRemoteService(config.Origin, config.CredentialFile)
	if err != nil {
		return err
	}
	var diagnostics io.Writer = log.Writer()
	if config.quiet {
		diagnostics = io.Discard
	}
	logger := log.New(diagnostics, "", log.LstdFlags)
	capture := config.capture
	if capture == nil {
		capture, err = startWaymoteWithDiagnostics(ctx, config.Waymote, diagnostics)
		if err != nil {
			return err
		}
		defer capture.close()
	}
	socket, err := service.socket(ctx)
	if err != nil {
		return err
	}
	defer socket.CloseNow()
	output := make(chan remoteMessage, 128)
	events := make(chan hostEvent, 128)
	failures := make(chan error, 1)
	readerDone := make(chan struct{})
	var readerError error // Read only after readerDone closes.
	fail := func(err error) {
		select {
		case failures <- err:
		default:
		}
		cancel()
	}
	emit := func(event hostEvent) {
		select {
		case events <- event:
		case <-ctx.Done():
		default:
			fail(errors.New("remote input capacity exceeded"))
		}
	}
	send := func(message remoteMessage) {
		select {
		case output <- message:
		case <-ctx.Done():
		default:
			fail(errors.New("remote signaling capacity exceeded"))
		}
	}
	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			case message := <-output:
				data, err := json.Marshal(message)
				if err != nil {
					fail(err)
					return
				}
				writeContext, done := context.WithTimeout(ctx, 10*time.Second)
				err = socket.Write(writeContext, websocket.MessageText, data)
				done()
				if err != nil {
					fail(publisherSocketError(err, "remote signaling write failed"))
					return
				}
			}
		}
	}()
	go func() {
		defer close(readerDone)
		for {
			kind, data, err := socket.Read(ctx)
			if err != nil {
				readerError = publisherSocketError(err, "remote signaling closed")
				fail(readerError)
				return
			}
			var message remoteMessage
			if kind != websocket.MessageText || json.Unmarshal(data, &message) != nil {
				fail(errors.New("invalid remote signaling"))
				return
			}
			emit(hostEvent{message: &message})
		}
	}()
	var motionMu sync.Mutex
	motions := map[string]hostEvent{}
	motionReady := make(chan struct{}, 1)
	queueMotion := func(event hostEvent) {
		motionMu.Lock()
		motions[event.viewer] = event
		motionMu.Unlock()
		select {
		case motionReady <- struct{}{}:
		default:
		}
	}
	peers := map[string]*hostPeer{}
	preparations := map[string]context.CancelFunc{}
	lease := controlLease{}
	var agent *agentJob
	finishAgent := func(result agentResult) {
		if agent == nil {
			return
		}
		job := agent
		agent = nil
		job.cancel()
		if lease.owner == job.owner && lease.generation == job.generation {
			if err := capture.releaseAll(); err != nil {
				fail(err)
			}
		}
		send(remoteMessage{Type: "agent_result", RequestID: job.id, agentResult: &result})
	}
	sendControl := func(peer *hostPeer, message controlMessage) error {
		if peer != nil && peer.frames {
			data, err := json.Marshal(message)
			if err != nil {
				return err
			}
			send(remoteMessage{Type: "control", ViewerID: peer.viewerID, Data: data})
			return nil
		}
		if peer == nil || peer.control.ReadyState() != webrtc.DataChannelStateOpen || peer.control.BufferedAmount() > 32768 {
			return errors.New("control channel unavailable")
		}
		data, err := json.Marshal(message)
		if err != nil {
			return err
		}
		return peer.control.SendText(string(data))
	}
	release := func() {
		owner := lease.owner
		lease = controlLease{}
		if agent != nil && agent.owner == owner {
			finishAgent(agentResult{Status: "cancelled"})
		}
		if err := capture.releaseAll(); err != nil {
			fail(err)
		}
		if owner != "" {
			_ = sendControl(peers[owner], controlMessage{Type: "revoked"})
		}
	}
	remove := func(id string) {
		if cancel := preparations[id]; cancel != nil {
			cancel()
			delete(preparations, id)
		}
		if lease.owner == id {
			release()
		}
		if peer := peers[id]; peer != nil {
			if peer.renewal != nil {
				peer.renewal()
			}
			delete(peers, id)
			if peer.connection != nil {
				_ = peer.connection.Close()
			}
			send(remoteMessage{Type: "close_viewer", ViewerID: id})
		}
		motionMu.Lock()
		delete(motions, id)
		motionMu.Unlock()
	}
	defer func() {
		finishAgent(agentResult{Status: "cancelled"})
		release()
		for id := range peers {
			remove(id)
		}
	}()
	apply := func(event hostEvent) {
		peer := peers[event.viewer]
		if peer == nil {
			return
		}
		now := time.Now()
		if now.Sub(event.created) > 500*time.Millisecond {
			if !event.motion {
				remove(event.viewer)
			}
			return
		}
		if lease.expired(now) {
			release()
		}
		if input, err := decodeInput(event.input); err == nil {
			if (input.Kind == "move") != event.motion {
				remove(event.viewer)
				return
			}
			accept, err := lease.accept(event.viewer, input, now)
			if err != nil {
				remove(event.viewer)
				return
			}
			if accept {
				if err = capture.apply(input); err != nil {
					fail(err)
				}
			}
			return
		}
		if event.motion {
			remove(event.viewer)
			return
		}
		var message controlMessage
		decoder := json.NewDecoder(bytes.NewReader(event.input))
		decoder.DisallowUnknownFields()
		if decoder.Decode(&message) != nil || decoder.Decode(new(any)) != io.EOF {
			remove(event.viewer)
			return
		}
		switch message.Type {
		case "acquire":
			if message.Generation != "" {
				remove(event.viewer)
				return
			}
			if strings.HasPrefix(lease.owner, "agent:") {
				release()
			}
			if lease.owner != "" {
				_ = sendControl(peer, controlMessage{Type: "denied"})
				return
			}
			if err := capture.releaseAll(); err != nil {
				fail(err)
				return
			}
			if sendControl(peer, controlMessage{Type: "granted", Generation: lease.acquire(event.viewer, now)}) != nil {
				remove(event.viewer)
			}
		case "renew":
			if lease.renew(event.viewer, message.Generation, now) != nil {
				remove(event.viewer)
			}
		case "release":
			if lease.owner == event.viewer && lease.generation == message.Generation {
				release()
			}
		default:
			remove(event.viewer)
		}
	}
	offer := func(id string, peer *hostPeer, restart bool) error {
		peer.answered = false
		peer.answerDeadline = time.Now().Add(25 * time.Second)
		value, err := peer.connection.CreateOffer(&webrtc.OfferOptions{ICERestart: restart})
		if err != nil {
			return err
		}
		if err = peer.connection.SetLocalDescription(value); err != nil {
			return err
		}
		send(remoteMessage{Type: "signal", ViewerID: id, Signal: &remoteSignal{Type: "offer", SDP: value.SDP}})
		peer.renewAt = time.Now().Add(config.ICERenewalInterval)
		return nil
	}
	add := func(id string, ice []webrtc.ICEServer) error {
		if peers[id] != nil {
			return nil
		}
		if len(peers) >= 4 {
			return errors.New("remote viewer capacity reached")
		}
		settings := webrtc.SettingEngine{}
		settings.SetIncludeLoopbackCandidate(config.IncludeLoopback)
		connection, err := webrtc.NewAPI(webrtc.WithSettingEngine(settings)).NewPeerConnection(webrtc.Configuration{ICEServers: ice})
		if err != nil {
			return err
		}
		peer := &hostPeer{connection: connection}
		peers[id] = peer
		sender, err := connection.AddTrack(capture.track)
		if err != nil {
			remove(id)
			return err
		}
		go func() {
			buffer := make([]byte, 1500)
			for {
				if _, _, err := sender.Read(buffer); err != nil {
					return
				}
			}
		}()
		ordered := false
		retransmits := uint16(0)
		peer.control, err = connection.CreateDataChannel("remote-control-v1", nil)
		if err != nil {
			remove(id)
			return err
		}
		motion, err := connection.CreateDataChannel("remote-motion-v1", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &retransmits})
		if err != nil {
			remove(id)
			return err
		}
		for _, channel := range []*webrtc.DataChannel{peer.control, motion} {
			isMotion := channel == motion
			channel.OnMessage(func(message webrtc.DataChannelMessage) {
				if !message.IsString || len(message.Data) > 8192 {
					emit(hostEvent{viewer: id, closed: true})
					return
				}
				event := hostEvent{viewer: id, input: append([]byte(nil), message.Data...), motion: isMotion, created: time.Now()}
				if isMotion {
					queueMotion(event)
				} else {
					emit(event)
				}
			})
			channel.OnClose(func() { emit(hostEvent{viewer: id, closed: true}) })
		}
		connection.OnDataChannel(func(channel *webrtc.DataChannel) { _ = channel.Close(); emit(hostEvent{viewer: id, closed: true}) })
		connection.OnICECandidate(func(candidate *webrtc.ICECandidate) {
			if candidate == nil {
				return
			}
			value := candidate.ToJSON()
			// The event loop sends the offer before processing its candidates,
			// including when CreateOffer itself starts an ICE restart.
			emit(hostEvent{viewer: id, localCandidate: &remoteSignal{Type: "candidate", Candidate: value.Candidate, SDPMid: value.SDPMid, SDPMLineIndex: value.SDPMLineIndex}})
		})
		connection.OnConnectionStateChange(func(state webrtc.PeerConnectionState) {
			logger.Printf("Remote peer %s", state)
			if state == webrtc.PeerConnectionStateFailed || state == webrtc.PeerConnectionStateClosed || state == webrtc.PeerConnectionStateDisconnected {
				emit(hostEvent{viewer: id, closed: true})
			}
		})
		connection.OnICEConnectionStateChange(func(state webrtc.ICEConnectionState) { logger.Printf("Remote ICE %s", state) })
		connection.SCTP().Transport().OnStateChange(func(state webrtc.DTLSTransportState) { logger.Printf("Remote DTLS %s", state) })
		if err = offer(id, peer, false); err != nil {
			remove(id)
			return err
		}
		return nil
	}
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	agentTick := time.NewTicker(16 * time.Millisecond)
	defer agentTick.Stop()
	frameTick := time.NewTicker(100 * time.Millisecond)
	defer frameTick.Stop()
	frameInFlight := false
	lastAuthorization := time.Now()
	lastRenewal := time.Now()
	connectionID := ""
	publication := ""
	for {
		select {
		case <-ctx.Done():
			// A concurrent write can fail as the close frame is read. Preserve
			// the broker's terminal replacement reason over that generic failure.
			<-readerDone
			if errors.Is(readerError, errRemoteHostReplaced) {
				return readerError
			}
			select {
			case err := <-failures:
				return err
			default:
				return ctx.Err()
			}
		case <-capture.done:
			return errors.New("Wayland capture stopped")
		case <-frameTick.C:
			if !config.Frames || frameInFlight {
				continue
			}
			for _, peer := range peers {
				if peer.frames && peer.framePending {
					frameInFlight = true
					go func() { frame := snapshotDesktop(ctx, config.Width, config.Height); emit(hostEvent{frame: &frame}) }()
					break
				}
			}
		case now := <-agentTick.C:
			if agent == nil {
				continue
			}
			if now.After(agent.deadline) {
				finishAgent(agentResult{Status: "cancelled"})
				continue
			}
			for agent != nil && agent.next < len(agent.steps) && !now.Before(agent.nextAt) {
				if now.Sub(agent.nextAt) > 500*time.Millisecond {
					finishAgent(agentResult{Status: "cancelled"})
					break
				}
				step := agent.steps[agent.next]
				if accepted, err := lease.accept(agent.owner, step.input, now); err != nil || !accepted {
					finishAgent(agentResult{Status: "busy"})
					break
				}
				if err := capture.apply(step.input); err != nil {
					finishAgent(agentResult{Status: "unavailable"})
					break
				}
				agent.next++
				if agent.next < len(agent.steps) {
					// Keep the intended gesture clock: rounding each delay up to
					// the next host tick accumulates visible drag latency.
					agent.nextAt = agent.nextAt.Add(agent.steps[agent.next].delay)
				}
			}
			if agent != nil && agent.next == len(agent.steps) && !agent.snapshotStarted {
				agent.snapshotStarted = true
				job := agent
				go func() {
					if len(job.steps) > 0 {
						select {
						case <-time.After(100 * time.Millisecond):
						case <-job.ctx.Done():
							return
						}
					}
					result := snapshotDesktop(job.ctx, config.Width, config.Height)
					emit(hostEvent{agentRequest: job.id, agentResult: &result})
				}()
			}
		case <-tick.C:
			for id, peer := range peers {
				if peer.frames {
					continue
				}
				if !peer.answered && time.Now().After(peer.answerDeadline) {
					remove(id)
					continue
				}
				if peer.answered && peer.renewal == nil && time.Now().After(peer.renewAt) {
					renewContext, cancel := context.WithCancel(ctx)
					peer.renewal = cancel
					go func() {
						ice, err := service.ice(renewContext)
						emit(hostEvent{viewer: id, refresh: true, ice: ice, err: err})
					}()
				}
			}
			if lease.expired(time.Now()) {
				release()
			}
			if time.Since(lastAuthorization) > 25*time.Second {
				return errors.New("remote authorization expired")
			}
			if connectionID != "" && time.Since(lastRenewal) >= 10*time.Second {
				lastRenewal = time.Now()
				id := connectionID
				go func() {
					if err := service.request(ctx, "/renew", map[string]string{"connection_id": id}, nil); err != nil {
						fail(err)
					}
				}()
			}
		case <-motionReady:
			motionMu.Lock()
			batch := motions
			motions = map[string]hostEvent{}
			motionMu.Unlock()
			for _, event := range batch {
				apply(event)
			}
		case event := <-events:
			if event.frame != nil {
				frameInFlight = false
				for id, peer := range peers {
					if !peer.frames || !peer.framePending {
						continue
					}
					peer.framePending = false
					if event.frame.Status != "ok" {
						remove(id)
						continue
					}
					frame := *event.frame
					frame.Status = ""
					send(remoteMessage{Type: "frame", ViewerID: id, agentResult: &frame})
				}
				continue
			}
			if event.agentResult != nil {
				if agent != nil && agent.id == event.agentRequest {
					finishAgent(*event.agentResult)
				}
				continue
			}
			if event.localCandidate != nil {
				if peers[event.viewer] != nil {
					send(remoteMessage{Type: "signal", ViewerID: event.viewer, Signal: event.localCandidate})
				}
				continue
			}
			if event.refresh {
				if peer := peers[event.viewer]; peer != nil && peer.renewal != nil {
					peer.renewal()
					peer.renewal = nil
					configuration := peer.connection.GetConfiguration()
					configuration.ICEServers = event.ice
					if event.err != nil || peer.connection.SetConfiguration(configuration) != nil || offer(event.viewer, peer, true) != nil {
						remove(event.viewer)
					}
				}
				continue
			}
			if event.prepared {
				if cancel := preparations[event.viewer]; cancel != nil {
					cancel()
					delete(preparations, event.viewer)
					if event.err != nil || add(event.viewer, event.ice) != nil {
						logger.Print("Could not prepare remote viewer")
						send(remoteMessage{Type: "close_viewer", ViewerID: event.viewer})
					}
				}
				continue
			}
			if event.closed {
				remove(event.viewer)
				continue
			}
			if event.message == nil {
				apply(event)
				continue
			}
			message := event.message
			switch message.Type {
			case "ready":
				if connectionID != "" || message.ConnectionID == "" {
					return errors.New("invalid remote connection")
				}
				connectionID = message.ConnectionID
				lastAuthorization = time.Now()
				kind := "vm"
				if strings.HasPrefix(service.base.Path, "/v1/hand-hosts/") {
					kind = "desktop"
				}
				transport := ""
				if config.Frames {
					transport = "frames-v1"
				}
				send(remoteMessage{Type: "catalog", MachineID: config.MachineID, MachineName: config.Name, Surfaces: []remoteSurface{{ID: "desktop", Name: "Desktop", Kind: kind, Width: config.Width, Height: config.Height, Controllable: true, AgentTools: true, Transport: transport}}})
			case "renewed":
				lastAuthorization = time.Now()
			case "published":
				publication = message.Generation
				if config.published != nil {
					config.published()
				}
				if !config.quiet {
					logger.Print("Wayland screen available for this account")
				}
			case "agent_cancel":
				if agent != nil && agent.id == message.RequestID {
					finishAgent(agentResult{Status: "cancelled"})
				}
			case "agent_call":
				reply := func(status string) {
					send(remoteMessage{Type: "agent_result", RequestID: message.RequestID, agentResult: &agentResult{Status: status}})
				}
				now := time.Now()
				if message.Input == nil || message.SurfaceID != "desktop" || message.Generation != publication ||
					!regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$`).MatchString(message.AgentID) ||
					!regexp.MustCompile(`^[0-9a-f-]{36}$`).MatchString(message.RequestID) ||
					message.DeadlineAt <= now.UnixMilli() || message.DeadlineAt > now.Add(10*time.Second).UnixMilli() {
					reply("invalid")
					continue
				}
				if agent != nil {
					reply("busy")
					continue
				}
				owner := "agent:" + message.AgentID
				if message.Input.Action == "release" {
					if lease.owner == owner {
						release()
					}
					reply("ok")
					continue
				}
				steps, err := message.Input.steps("pending")
				if err != nil {
					reply("invalid")
					continue
				}
				generation := ""
				if len(steps) > 0 {
					if lease.expired(now) || lease.owner == owner {
						release()
					}
					if lease.owner != "" {
						reply("busy")
						continue
					}
					if err := capture.releaseAll(); err != nil {
						reply("unavailable")
						continue
					}
					generation = lease.acquire(owner, now)
					for i := range steps {
						steps[i].input.Generation = generation
					}
				}
				jobCtx, cancelJob := context.WithDeadline(ctx, time.UnixMilli(message.DeadlineAt))
				agent = &agentJob{id: message.RequestID, owner: owner, generation: generation, steps: steps,
					deadline: time.UnixMilli(message.DeadlineAt), nextAt: now, ctx: jobCtx, cancel: cancelJob}
			case "viewer":
				if !config.quiet {
					logger.Print("Preparing remote viewer")
				}
				if message.ViewerID == "" || message.SurfaceID != "desktop" {
					return errors.New("invalid viewer request")
				}
				if peers[message.ViewerID] != nil || preparations[message.ViewerID] != nil {
					continue
				}
				if len(peers)+len(preparations) >= 4 {
					send(remoteMessage{Type: "close_viewer", ViewerID: message.ViewerID})
					continue
				}
				if config.Frames {
					peers[message.ViewerID] = &hostPeer{viewerID: message.ViewerID, frames: true, answered: true}
					continue
				}
				// Fetch fresh TURN credentials without blocking input from existing
				// viewers. A departing viewer cancels its pending request.
				prepareContext, cancel := context.WithCancel(ctx)
				id := message.ViewerID
				preparations[id] = cancel
				go func() {
					ice, err := service.ice(prepareContext)
					emit(hostEvent{viewer: id, prepared: true, ice: ice, err: err})
				}()
			case "viewer_left":
				remove(message.ViewerID)
			case "frame_request":
				if peer := peers[message.ViewerID]; peer != nil && peer.frames {
					peer.framePending = true
				}
			case "input", "control":
				if peer := peers[message.ViewerID]; peer != nil && peer.frames {
					if len(message.Data) > 8192 {
						remove(message.ViewerID)
						continue
					}
					input, _ := decodeInput(message.Data)
					apply(hostEvent{viewer: message.ViewerID, input: message.Data, created: time.Now(), motion: input.Kind == "move"})
				}
			case "signal":
				peer := peers[message.ViewerID]
				if peer == nil || peer.frames || message.Signal == nil {
					continue
				}
				signal := message.Signal
				switch signal.Type {
				case "answer":
					if peer.answered || len(signal.SDP) > 65536 {
						remove(message.ViewerID)
						continue
					}
					if err = peer.connection.SetRemoteDescription(webrtc.SessionDescription{Type: webrtc.SDPTypeAnswer, SDP: signal.SDP}); err != nil {
						remove(message.ViewerID)
						continue
					}
					peer.answered = true
					for _, candidate := range peer.candidates {
						if err = peer.connection.AddICECandidate(candidate); err != nil {
							break
						}
					}
					peer.candidates = nil
				case "candidate":
					candidate := webrtc.ICECandidateInit{Candidate: signal.Candidate, SDPMid: signal.SDPMid, SDPMLineIndex: signal.SDPMLineIndex}
					if peer.answered {
						err = peer.connection.AddICECandidate(candidate)
					} else if len(peer.candidates) < 128 {
						peer.candidates = append(peer.candidates, candidate)
					} else {
						err = errors.New("too many ICE candidates")
					}
				default:
					err = errors.New("invalid peer signal")
				}
				if err != nil {
					remove(message.ViewerID)
					err = nil
				}
			default:
				return errors.New("invalid host signaling message")
			}
		}
	}
}
