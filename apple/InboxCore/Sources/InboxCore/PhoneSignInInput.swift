import Foundation
import PhoneNumberKit

public enum PhoneNumberInput {
    private static let utility = PhoneNumberUtility()

    public struct Country: Identifiable, Sendable {
        public let id: String
        public let name: String
        public let callingCode: String
        public var label: String { "\(name) \(callingCode)" }
    }

    public static let countries: [Country] = utility.allCountries().compactMap { region in
        guard region.count == 2, let code = utility.countryCode(for: region) else { return nil }
        return Country(id: region, name: Locale.current.localizedString(forRegionCode: region) ?? region, callingCode: "+\(code)")
    }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }

    public static func defaultRegion(locale: Locale = .current) -> String {
        let region = locale.region?.identifier.uppercased() ?? "US"
        return countries.contains { $0.id == region } ? region : "US"
    }

    public static func example(region: String) -> String {
        utility.getFormattedExampleNumber(forCountry: region, withFormat: .national) ?? "Phone number"
    }

    /// Parse national numbers using the visible country selection. Explicit +
    /// or 00 prefixes take precedence, so a pasted international number is safe.
    public static func normalize(_ input: String, region: String) throws -> String {
        var number = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard number.range(of: "^[+0-9\\s().-]+$", options: .regularExpression) != nil else { throw invalidPhone }
        number = number.replacingOccurrences(of: "[\\s().-]", with: "", options: .regularExpression)
        guard number.range(of: "^\\+?[0-9]+$", options: .regularExpression) != nil else { throw invalidPhone }
        if number.hasPrefix("00") { number = "+" + number.dropFirst(2) }
        guard let parsed = try? utility.parse(number, withRegion: region, ignoreType: true), parsed.numberExtension == nil else { throw invalidPhone }
        let normalized = utility.format(parsed, toType: .e164)
        guard normalized.range(of: "^\\+[1-9][0-9]{7,14}$", options: .regularExpression) != nil else { throw invalidPhone }
        return normalized
    }

    private static var invalidPhone: SMSAuthError {
        SMSAuthError(code: "invalid_phone", message: "Enter a valid phone number and check the selected country.")
    }
}

/// Complete pasted, autofilled, and typed codes use the same submission policy.
/// A failed code stays editable and can be retried explicitly, without a loop.
public struct SMSCodeInput {
    public private(set) var text = ""
    private var submitted: String?
    public init() {}

    public mutating func update(_ input: String, canSubmit: Bool) -> String? {
        text = String(input.filter { $0.isASCII && $0.isNumber })
        guard text.count == 6, canSubmit, submitted != text else { return nil }
        submitted = text
        return text
    }

    public mutating func markSubmitted() { submitted = text }
}
