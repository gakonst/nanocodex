import Foundation

/// Keyboard keys on the wire are USB HID keyboard usages (page 0x07), shared
/// by UIKit, browser clients, and Linux. Text is sent separately after IME composition.
public enum RemoteKey {
    public static let macToHID: [UInt16: UInt16] = [
        0:4, 11:5, 8:6, 2:7, 14:8, 3:9, 5:10, 4:11, 34:12, 38:13, 40:14, 37:15,
        46:16, 45:17, 31:18, 35:19, 12:20, 15:21, 1:22, 17:23, 32:24, 9:25, 13:26,
        7:27, 16:28, 6:29, 18:30, 19:31, 20:32, 21:33, 23:34, 22:35, 26:36, 28:37,
        25:38, 29:39, 36:40, 53:41, 51:42, 48:43, 49:44, 27:45, 24:46, 33:47,
        30:48, 42:49, 41:51, 39:52, 50:53, 43:54, 47:55, 44:56, 57:57,
        122:58, 120:59, 99:60, 118:61, 96:62, 97:63, 98:64, 100:65, 101:66, 109:67,
        103:68, 111:69, 114:73, 115:74, 116:75, 117:76, 119:77, 121:78, 124:79,
        123:80, 125:81, 126:82, 71:83, 75:84, 67:85, 78:86, 69:87, 76:88,
        83:89, 84:90, 85:91, 86:92, 87:93, 88:94, 89:95, 91:96, 92:97, 82:98,
        65:99, 10:100, 81:103, 59:224, 56:225, 58:226, 55:227, 62:228, 60:229,
        61:230, 54:231,
    ]
    public static let hidToMac = Dictionary(uniqueKeysWithValues: macToHID.map { ($0.value, $0.key) })
    public static func supported(_ usage: UInt16) -> Bool { hidToMac[usage] != nil }
}
