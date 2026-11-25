import Foundation
import Observation
import Network

/// “我的设备”里的一台设备：连接成功时自动保存，下次点一下就连
struct SavedDevice: Codable, Equatable, Identifiable {
    var code: String
    /// 对方报告的名称
    var name: String
    /// 用户起的名字（优先显示）
    var alias: String = ""
    var hostOs: String = ""
    var lastConnected: Double = 0
    /// 重连凭证（32 位十六进制）
    var resume: String = ""
    /// 对方的端到端加密公钥（64 位十六进制）：之后只认它
    var hostKey: String? = nil
    var lanAddr: String? = nil
    var lanResume: String? = nil

    var id: String { code }
    var title: String { !alias.isEmpty ? alias : (!name.isEmpty ? name : (lanAddr ?? Code.format(code))) }
    var os: OsKind { OsKind.of(hostOs) }
}

@Observable
final class DeviceStore {
    private(set) var list: [SavedDevice] = []
    private let file = "devices.json"

    init() {
        list = SharedStore.read(file, as: [SavedDevice].self) ?? []
        sort()
    }

    func get(_ code: String) -> SavedDevice? { list.first { $0.code == code } }

    func getLan(_ address: String) -> SavedDevice? {
        guard let key = Self.lanKey(address) else { return nil }
        return list.first { $0.lanAddr.flatMap(Self.lanKey) == key }
    }

    /// Match numeric endpoint representations so manual dialing also reuses the saved key.
    private static func lanKey(_ address: String) -> String? {
        let text = address.trimmingCharacters(in: .whitespacesAndNewlines)
        let host: String
        let port: Int
        if text.hasPrefix("[") {
            guard let end = text.firstIndex(of: "]") else { return nil }
            host = String(text[text.index(after: text.startIndex)..<end])
            let tail = String(text[text.index(after: end)...])
            if tail.isEmpty { port = 4434 }
            else {
                guard tail.hasPrefix(":"), let value = Int(tail.dropFirst()) else { return nil }
                port = value
            }
        } else if text.filter({ $0 == ":" }).count == 1 {
            let parts = text.split(separator: ":", omittingEmptySubsequences: false)
            guard let value = Int(parts[1]) else { return nil }
            host = String(parts[0])
            port = value
        } else {
            host = text
            port = 4434
        }
        guard (1...65535).contains(port),
              var data = IPv4Address(host)?.rawValue ?? IPv6Address(host)?.rawValue else { return nil }
        let bytes = Array(data)
        if bytes.count == 16, bytes.prefix(10).allSatisfy({ $0 == 0 }), bytes[10] == 255, bytes[11] == 255 {
            data = Data(bytes.suffix(4))
        }
        return data.map { String(format: "%02x", Int($0)) }.joined() + ":\(port)"
    }

    /// 开始会话时记下（名称、系统、凭证、对方公钥）
    func connected(code: String, name: String, os: String, resume: String, hostKey: String?, lanAddr: String? = nil) {
        let old = lanAddr != nil && hostKey != nil ? list.first { $0.hostKey == hostKey } ?? get(code) : get(code)
        var d = old ?? SavedDevice(code: code, name: name)
        d.code = code
        if !name.isEmpty { d.name = name }
        if !os.isEmpty { d.hostOs = os }
        if let lanAddr {
            d.lanAddr = lanAddr
            d.lanResume = resume
        } else if !resume.isEmpty { d.resume = resume }
        if let hostKey, !hostKey.isEmpty { d.hostKey = hostKey }
        d.lastConnected = Date().timeIntervalSince1970
        list.removeAll { $0.code == code || $0.code == old?.code }
        list.append(d)
        save()
    }

    func rename(_ code: String, to alias: String) {
        guard let i = list.firstIndex(where: { $0.code == code }) else { return }
        list[i].alias = alias.trimmingCharacters(in: .whitespacesAndNewlines)
        save()
    }

    func remove(_ code: String) {
        list.removeAll { $0.code == code }
        save()
    }

    func restore(_ d: SavedDevice) {
        list.removeAll { $0.code == d.code }
        list.append(d)
        save()
    }

    func clear() {
        list = []
        save()
    }

    private func sort() { list.sort { $0.lastConnected > $1.lastConnected } }

    private func save() {
        sort()
        SharedStore.write(file, list)
    }
}
