package main

import (
	"howett.net/plist"
	"os"
	"path/filepath"
	"testing"
)

func TestPhoneRunnerConfigurationKeepsPortsPrivateAndSourceUnchanged(t *testing.T) {
	directory := t.TempDir()
	source := filepath.Join(directory, "source.xctestrun")
	destination := filepath.Join(directory, "prepared.xctestrun")
	config := map[string]any{"__xctestrun_metadata__": map[string]any{"FormatVersion": 1}, "WebDriverAgentRunner": map[string]any{
		"ProductModuleName": "WebDriverAgentRunner", "TestHostPath": "__TESTROOT__/Runner.app",
		"EnvironmentVariables": map[string]any{"USE_IP": "0.0.0.0", "USE_PORT": "8100", "MJPEG_SERVER_PORT": "9100"},
	}}
	data, err := plist.Marshal(config, plist.XMLFormat)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(source, data, 0600); err != nil {
		t.Fatal(err)
	}
	if err = preparePhoneRunner(source, destination); err != nil {
		t.Fatal(err)
	}
	original, _ := os.ReadFile(source)
	if string(original) != string(data) {
		t.Fatal("modified the signed build configuration")
	}
	prepared, _ := os.ReadFile(destination)
	if _, err = plist.Unmarshal(prepared, &config); err != nil {
		t.Fatal(err)
	}
	target := config["WebDriverAgentRunner"].(map[string]any)
	if target["TestHostPath"] != filepath.Join(directory, "Runner.app") {
		t.Fatal("build root was not preserved")
	}
	env := target["EnvironmentVariables"].(map[string]any)
	if env["USE_IP"] != "127.0.0.1" || env["USE_PORT"] != "18100" || env["MJPEG_SERVER_PORT"] != "19100" {
		t.Fatal("runner must use private expected ports")
	}
	config["UnrelatedTests"] = map[string]any{}
	data, _ = plist.Marshal(config, plist.XMLFormat)
	_ = os.WriteFile(source, data, 0600)
	if err = preparePhoneRunner(source, destination); err == nil {
		t.Fatal("accepted an unrelated test target")
	}
}
