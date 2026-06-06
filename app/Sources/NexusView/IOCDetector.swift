import Foundation

/// An indicator of compromise recognized in a cell value.
enum IOCType {
    case md5, sha1, sha256, ipv4, ipv6

    var label: String {
        switch self {
        case .md5: return "MD5 hash"
        case .sha1: return "SHA-1 hash"
        case .sha256: return "SHA-256 hash"
        case .ipv4, .ipv6: return "IP address"
        }
    }

    /// VirusTotal GUI path segment for this indicator type.
    var virusTotalPath: String {
        switch self {
        case .md5, .sha1, .sha256: return "file"
        case .ipv4, .ipv6: return "ip-address"
        }
    }
}

/// Recognizes hashes and IP addresses in cell values and builds enrichment URLs.
/// Conservative on purpose (no domain heuristic) so values like `explorer.exe`
/// are never misclassified.
enum IOCDetector {
    static func detect(_ raw: String) -> (type: IOCType, value: String)? {
        let v = raw.trimmingCharacters(in: .whitespaces)
        guard !v.isEmpty else { return nil }
        if isHex(v, length: 32) { return (.md5, v.lowercased()) }
        if isHex(v, length: 40) { return (.sha1, v.lowercased()) }
        if isHex(v, length: 64) { return (.sha256, v.lowercased()) }
        if isIPv4(v) { return (.ipv4, v) }
        if isIPv6(v) { return (.ipv6, v) }
        return nil
    }

    /// VirusTotal GUI lookup URL for an indicator (opened in the browser).
    static func virusTotalURL(type: IOCType, value: String) -> URL? {
        guard let encoded = value.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) else { return nil }
        return URL(string: "https://www.virustotal.com/gui/\(type.virusTotalPath)/\(encoded)")
    }

    private static func isHex(_ s: String, length: Int) -> Bool {
        s.count == length && s.allSatisfy(\.isHexDigit)
    }

    private static func isIPv4(_ s: String) -> Bool {
        let parts = s.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4 else { return false }
        return parts.allSatisfy { part in
            !part.isEmpty && part.count <= 3 && part.allSatisfy(\.isNumber) && (Int(part) ?? 256) <= 255
        }
    }

    private static func isIPv6(_ s: String) -> Bool {
        guard s.contains(":"), s.filter({ $0 == ":" }).count >= 2 else { return false }
        let allowed = CharacterSet(charactersIn: "0123456789abcdefABCDEF:")
        return s.unicodeScalars.allSatisfy { allowed.contains($0) }
    }
}
