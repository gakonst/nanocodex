#!/usr/bin/env python3
from pathlib import Path
import plistlib


ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "apple" / "NanocodexInbox"
ROLE = "CPTemplateApplicationSceneSessionRoleApplication"
ENTITLEMENT = "com.apple.developer.carplay-voice-based-conversation"


with (APP / "Info.iOS.plist").open("rb") as source:
    info = plistlib.load(source)
configurations = info["UIApplicationSceneManifest"]["UISceneConfigurations"]
carplay = configurations[ROLE]
assert len(carplay) == 1
assert carplay[0]["UISceneClassName"] == "CPTemplateApplicationScene"
assert carplay[0]["UISceneDelegateClassName"] == "$(PRODUCT_MODULE_NAME).CarPlaySceneDelegate"
assert "audio" in info["UIBackgroundModes"]

with (APP / "NanocodexInbox.iOS.entitlements").open("rb") as source:
    entitlements = plistlib.load(source)
assert entitlements.get(ENTITLEMENT) is True

project = (ROOT / "apple" / "NanocodexInbox.xcodeproj" / "project.pbxproj").read_text()
assert 'path = "NanocodexInbox/CarPlaySceneDelegate.swift"' in project
assert project.count("INFOPLIST_KEY_UIApplicationSceneManifest_Generation = NO") == 2

source = (APP / "CarPlaySceneDelegate.swift").read_text()
assert "CPVoiceControlTemplate" in source
assert "presentTemplate" in source
assert "templateDidDisappear" in source
print("CarPlay voice scene contract is configured")
