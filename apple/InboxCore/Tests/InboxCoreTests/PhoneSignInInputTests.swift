import XCTest
@testable import InboxCore

final class PhoneSignInInputTests: XCTestCase {
    func testNationalNumbersUseSelectedCountryAndTrunkRules() throws {
        for (input, region, expected) in [
            ("(202) 555-0123", "US", "+12025550123"),
            ("1 202 555 0123", "US", "+12025550123"),
            ("697 123 4567", "GR", "+306971234567"),
            ("07911 123456", "GB", "+447911123456"),
            ("06 12 34 56 78", "FR", "+33612345678"),
            ("0412 345 678", "AU", "+61412345678"),
            ("06 6982 1234", "IT", "+390669821234"),
            ("090 1234 5678", "JP", "+819012345678"),
            ("11 91234-5678", "BR", "+5511912345678")
        ] {
            XCTAssertEqual(try PhoneNumberInput.normalize(input, region: region), expected, "\(input) / \(region)")
        }
    }

    func testExplicitInternationalNumbersOverrideCountryWithoutDoublePrefix() throws {
        for input in ["+1 (202) 555-0123", "0012025550123"] {
            XCTAssertEqual(try PhoneNumberInput.normalize(input, region: "GR"), "+12025550123")
        }
        XCTAssertEqual(try PhoneNumberInput.normalize("+44 7911 123456", region: "US"), "+447911123456")
        XCTAssertEqual(try PhoneNumberInput.normalize("011 44 7911 123456", region: "US"), "+447911123456")
        for input in ["", "555", "+", "++12025550123", "+12025550123 ext 4", "202555012312345678", "call 2025550123"] {
            XCTAssertThrowsError(try PhoneNumberInput.normalize(input, region: "US"), input)
        }
    }

    func testDefaultCountryUsesRegionRatherThanLanguage() {
        XCTAssertEqual(PhoneNumberInput.defaultRegion(locale: Locale(identifier: "en_GR")), "GR")
        XCTAssertEqual(PhoneNumberInput.defaultRegion(locale: Locale(identifier: "el_US")), "US")
        XCTAssertEqual(PhoneNumberInput.defaultRegion(locale: Locale(identifier: "en_GB")), "GB")
        XCTAssertEqual(PhoneNumberInput.defaultRegion(locale: Locale(identifier: "en_001")), "US")
        XCTAssertEqual(PhoneNumberInput.countries.first { $0.id == "GR" }?.callingCode, "+30")
        XCTAssertGreaterThan(PhoneNumberInput.countries.count, 200)
    }

    func testCompleteCodeSubmitsOnceAndAllowsChangedCodeOrFreshChallenge() {
        var input = SMSCodeInput()
        XCTAssertNil(input.update("12345", canSubmit: true))
        XCTAssertEqual(input.update("123 456", canSubmit: true), "123456")
        XCTAssertEqual(input.text, "123456")
        XCTAssertNil(input.update("123456", canSubmit: true), "Duplicate edit callbacks must not resubmit")
        XCTAssertNil(input.update("123456", canSubmit: true), "A failed request must wait for an explicit retry or different code")
        XCTAssertEqual(input.update("654321", canSubmit: true), "654321")
        input = SMSCodeInput()
        XCTAssertEqual(input.update("123456", canSubmit: true), "123456", "A new challenge permits the same six digits")
    }

    func testCodeDoesNotSubmitWhileBusyExpiredOrCoolingDownAndNeverTruncates() {
        var input = SMSCodeInput()
        XCTAssertNil(input.update("123456", canSubmit: false))
        XCTAssertEqual(input.text, "123456", "Keep code editable when automatic verification is unavailable")
        input.markSubmitted()
        XCTAssertNil(input.update("123456", canSubmit: true), "Manual retry also prevents duplicate automatic submissions")
        XCTAssertNil(input.update("1234567", canSubmit: true), "Never send the first six digits of an overlong paste")
        XCTAssertEqual(input.text, "1234567")
        XCTAssertNil(input.update("", canSubmit: true))
    }
}
