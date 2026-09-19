import SwiftUI
import Contacts
import CoreLocation
import Photos

@MainActor
final class DevicePermissions: NSObject, ObservableObject, CLLocationManagerDelegate {
    @Published var contacts = CNContactStore.authorizationStatus(for: .contacts)
    @Published var photos = PHPhotoLibrary.authorizationStatus(for: .readWrite)
    @Published var location: CLAuthorizationStatus = .notDetermined
    @Published var precise = false
    @Published var requesting = false
    @Published var error: String?
    private let locationManager = CLLocationManager()
    private let contactStore = CNContactStore()

    override init() {
        super.init()
        locationManager.delegate = self
        refresh()
    }

    func refresh() {
        contacts = CNContactStore.authorizationStatus(for: .contacts)
        photos = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        location = locationManager.authorizationStatus
        precise = locationManager.accuracyAuthorization == .fullAccuracy
    }

    func requestContacts() async {
        guard !requesting, contacts == .notDetermined else { return }
        requesting = true
        defer { requesting = false; refresh() }
        do { _ = try await contactStore.requestAccess(for: .contacts) }
        catch { self.error = "Contacts access could not be requested. You can review it in Settings." }
    }

    func requestPhotos() async {
        guard !requesting, photos == .notDetermined else { return }
        requesting = true
        _ = await PHPhotoLibrary.requestAuthorization(for: .readWrite)
        requesting = false
        refresh()
    }

    func requestLocation() {
        guard !requesting, location == .notDetermined else { return }
        locationManager.requestWhenInUseAuthorization()
    }

    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        Task { @MainActor [weak self] in self?.refresh() }
    }
}

struct DevicePermissionsView: View {
    @StateObject private var access = DevicePermissions()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Form {
            Section {
                permission("Contacts", icon: "person.crop.circle", status: contactsStatus,
                    unrequested: access.contacts == .notDetermined,
                    restricted: access.contacts == .restricted) {
                    Task { await access.requestContacts() }
                }
            } footer: {
                Text("Choose which contacts Nanocodex can access. iOS supports selected contacts or full access.")
            }
            Section {
                permission("Location", icon: "location", status: locationStatus,
                    unrequested: access.location == .notDetermined,
                    restricted: access.location == .restricted) {
                    access.requestLocation()
                }
            } footer: {
                Text("Include a recent location with new tasks and allow agents to request your current location while using Nanocodex. Choose approximate or precise location in iOS Settings.")
            }
            Section {
                permission("Photos", icon: "photo.on.rectangle", status: photosStatus,
                    unrequested: access.photos == .notDetermined,
                    restricted: access.photos == .restricted) {
                    Task { await access.requestPhotos() }
                }
            } footer: {
                Text("Choose selected photos or your full library. Attaching photos with the system picker also works without library access.")
            }
            Section {
                Text("Agents can search permitted contacts and photo metadata while this phone is connected as a Hand. New tasks include a recent location when location access is enabled.")
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Device access")
        .onChange(of: scenePhase) { _, phase in if phase == .active { access.refresh() } }
        .onAppear { access.refresh() }
        .alert("Permission request", isPresented: Binding(get: { access.error != nil }, set: { if !$0 { access.error = nil } })) {
            Button("OK") { access.error = nil }
        } message: { Text(access.error ?? "") }
    }

    private func permission(_ title: String, icon: String, status: String,
                            unrequested: Bool, restricted: Bool, request: @escaping () -> Void) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            LabeledContent { Text(status).foregroundStyle(.secondary) } label: { Label(title, systemImage: icon) }
            if restricted {
                Text("Access is restricted by this device’s settings.").font(.caption).foregroundStyle(.secondary)
            } else {
                Button(unrequested ? "Allow access" : "Manage in Settings") {
                    if unrequested { request() }
                    else if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                }
                .disabled(access.requesting)
                .accessibilityIdentifier("permission-" + title.lowercased())
            }
        }
    }

    private var contactsStatus: String {
        switch access.contacts {
        case .notDetermined: "Not requested"
        case .denied: "Not allowed"
        case .restricted: "Restricted"
        case .authorized: "Full access"
        case .limited: "Selected contacts"
        @unknown default: "Unknown"
        }
    }
    private var photosStatus: String {
        switch access.photos {
        case .notDetermined: "Not requested"
        case .denied: "Not allowed"
        case .restricted: "Restricted"
        case .authorized: "Full access"
        case .limited: "Selected photos"
        @unknown default: "Unknown"
        }
    }
    private var locationStatus: String {
        switch access.location {
        case .notDetermined: "Not requested"
        case .denied: "Not allowed"
        case .restricted: "Restricted"
        case .authorizedAlways: access.precise ? "Always · precise" : "Always · approximate"
        case .authorizedWhenInUse: access.precise ? "While using · precise" : "While using · approximate"
        @unknown default: "Unknown"
        }
    }
}
