package main

import (
	"bytes"
	"context"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"regexp"
	"strings"
	"time"

	"github.com/coder/websocket"
	"github.com/pion/webrtc/v4"
)

type remoteSignal struct {
	Type          string  `json:"type"`
	SDP           string  `json:"sdp,omitempty"`
	Candidate     string  `json:"candidate,omitempty"`
	SDPMid        *string `json:"sdpMid,omitempty"`
	SDPMLineIndex *uint16 `json:"sdpMLineIndex,omitempty"`
}
type remoteMessage struct {
	Type         string          `json:"type"`
	ConnectionID string          `json:"connection_id,omitempty"`
	ViewerID     string          `json:"viewer_id,omitempty"`
	SurfaceID    string          `json:"surface_id,omitempty"`
	MachineID    string          `json:"machine_id,omitempty"`
	MachineName  string          `json:"machine_name,omitempty"`
	Generation   string          `json:"generation,omitempty"`
	Surfaces     []remoteSurface `json:"surfaces,omitempty"`
	Signal       *remoteSignal   `json:"signal,omitempty"`
	RequestID    string          `json:"request_id,omitempty"`
	AgentID      string          `json:"agent_id,omitempty"`
	DeadlineAt   int64           `json:"deadline_at,omitempty"`
	Input        *agentInput     `json:"input,omitempty"`
	Data         json.RawMessage `json:"data,omitempty"`
	*agentResult
}
type remoteSurface struct {
	ID           string `json:"id"`
	Name         string `json:"name"`
	Kind         string `json:"kind"`
	Width        int    `json:"width"`
	Height       int    `json:"height"`
	Controllable bool   `json:"controllable"`
	AgentTools   bool   `json:"agent_tools,omitempty"`
	Transport    string `json:"transport,omitempty"`
}

var errRemoteHostReplaced = errors.New("remote host replaced by another publisher")

// Only the authenticated publisher socket can retire this daemon. Other policy
// closes (including authorization expiry during credential rotation) still retry.
func publisherSocketError(err error, fallback string) error {
	var closed websocket.CloseError
	if errors.As(err, &closed) && closed.Code == websocket.StatusPolicyViolation && closed.Reason == "Host replaced" {
		return errRemoteHostReplaced
	}
	return errors.New(fallback)
}

// A standalone Hand reads its credential from an owner-only file, never argv.
// Factory guests must receive an allocation-scoped credential from their owner;
// the VM launcher must not copy the user's account/provider credentials into them.
type remoteService struct {
	base   *url.URL
	token  string
	client *http.Client
}

func newRemoteService(origin, credentialPath string) (*remoteService, error) {
	base, err := url.Parse(origin)
	if err != nil || base.Host == "" || base.User != nil || base.RawQuery != "" || base.Fragment != "" {
		return nil, errors.New("invalid remote service origin")
	}
	loopback := base.Hostname() == "127.0.0.1" || base.Hostname() == "localhost" || base.Hostname() == "::1"
	if base.Scheme != "https" && !(base.Scheme == "http" && loopback) {
		return nil, errors.New("remote service requires HTTPS")
	}
	if base.Path == "" || base.Path == "/" {
		base.Path = "/v1/account/hands"
	} else if !(regexp.MustCompile(`^/v1/vm-host-attachments/[A-Za-z0-9_-]{43}/[0-9a-f-]{36}/hands$`).MatchString(base.Path) ||
		regexp.MustCompile(`^/v1/hand-hosts/[0-9a-f-]{36}/[0-9a-f-]{36}/hands$`).MatchString(base.Path)) || base.RawPath != "" {
		return nil, errors.New("invalid allocation remote endpoint")
	}
	file, err := os.Open(credentialPath)
	if err != nil {
		return nil, errors.New("cannot open remote credential file")
	}
	defer file.Close()
	stat, err := file.Stat()
	if err != nil || !stat.Mode().IsRegular() || stat.Mode().Perm()&0077 != 0 {
		return nil, errors.New("remote credential file must be private (0600)")
	}
	data, err := io.ReadAll(io.LimitReader(file, 8193))
	if err != nil || len(data) > 8192 {
		return nil, errors.New("invalid remote credential file")
	}
	token := strings.TrimSpace(string(data))
	if token == "" || strings.ContainsAny(token, "\r\n\t ") {
		return nil, errors.New("invalid remote credential")
	}
	return &remoteService{base: base, token: token, client: &http.Client{Timeout: 10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}, nil
}
func (service *remoteService) request(ctx context.Context, suffix string, body any, result any) error {
	data, err := json.Marshal(body)
	if err != nil {
		return err
	}
	endpoint := service.base.ResolveReference(&url.URL{Path: service.base.Path + suffix})
	request, err := http.NewRequestWithContext(ctx, "POST", endpoint.String(), bytes.NewReader(data))
	if err != nil {
		return err
	}
	request.Header.Set("Authorization", "Bearer "+service.token)
	request.Header.Set("Content-Type", "application/json")
	response, err := service.client.Do(request)
	if err != nil {
		return errors.New("remote service request failed")
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("remote service refused request (%d)", response.StatusCode)
	}
	data, err = io.ReadAll(io.LimitReader(response.Body, 131073))
	if err != nil || len(data) > 131072 {
		return errors.New("invalid remote service response")
	}
	if result != nil {
		return json.Unmarshal(data, result)
	}
	return nil
}
func (service *remoteService) ice(ctx context.Context) ([]webrtc.ICEServer, error) {
	var body struct {
		Servers []webrtc.ICEServer `json:"iceServers"`
	}
	if err := service.request(ctx, "/ice", map[string]any{}, &body); err != nil {
		return nil, err
	}
	return body.Servers, nil
}
func (service *remoteService) socket(ctx context.Context) (*websocket.Conn, error) {
	endpoint := service.base.ResolveReference(&url.URL{Path: service.base.Path + "/host"})
	if endpoint.Scheme == "https" {
		endpoint.Scheme = "wss"
	} else {
		endpoint.Scheme = "ws"
	}
	header := http.Header{}
	header.Set("Authorization", "Bearer "+service.token)
	connection, response, err := websocket.Dial(ctx, endpoint.String(), &websocket.DialOptions{HTTPClient: service.client, HTTPHeader: header})
	if err != nil {
		if response != nil && response.Body != nil {
			response.Body.Close()
		}
		if response != nil {
			return nil, fmt.Errorf("remote signaling refused (%d)", response.StatusCode)
		}
		var dns *net.DNSError
		var trust x509.UnknownAuthorityError
		if errors.As(err, &dns) {
			return nil, errors.New("remote signaling DNS lookup failed")
		}
		if errors.As(err, &trust) {
			return nil, errors.New("remote signaling certificate authority not trusted")
		}
		return nil, errors.New("remote signaling connection failed")
	}
	connection.SetReadLimit(70_000)
	return connection, nil
}
