// Credential-free fresh URLSession controls; expected finite409, never an admitted viewer.
import Foundation
final class Delegate: NSObject, URLSessionTaskDelegate {
  func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
    guard task.taskDescription?.hasPrefix("probe") == true else { return }
    let ts = metrics.transactionMetrics.map { m -> [String: Any] in
      func ms(_ a: Date?, _ b: Date?) -> Any { guard let a, let b else {return NSNull()}; return b.timeIntervalSince(a)*1000 }
      return ["sample":task.taskDescription ?? "", "protocol":m.networkProtocolName ?? "", "reuse":m.isReusedConnection,"dns_ms":ms(m.domainLookupStartDate,m.domainLookupEndDate),"connect_ms":ms(m.connectStartDate,m.connectEndDate),"tls_ms":ms(m.secureConnectionStartDate,m.secureConnectionEndDate),"request_ms":ms(m.requestStartDate,m.requestEndDate),"wait_ms":ms(m.requestEndDate,m.responseStartDate),"fetch_to_request_ms":ms(m.fetchStartDate,m.requestStartDate),"total_ms":ms(m.fetchStartDate,m.responseEndDate),"status":(m.response as? HTTPURLResponse)?.statusCode ?? 0,"request_id":(m.response as? HTTPURLResponse)?.value(forHTTPHeaderField:"x-nanocodex-request-id") ?? "","server_timing":(m.response as? HTTPURLResponse)?.value(forHTTPHeaderField:"server-timing") ?? ""]
    }
    print(String(data:try! JSONSerialization.data(withJSONObject:ts), encoding:.utf8)!)
  }
}
@main struct Main {
 static func main() async throws {
  print("started_at \(Date())")
  for index in 0..<4 {
   for ws in [false,true] {
    let session=URLSession(configuration:.ephemeral,delegate:Delegate(),delegateQueue:nil)
    let name="probe-\(index)-\(ws ? "ws" : "https")"
    let request=URLRequest(url:URL(string:"\(ws ? "wss" : "https")://nanocodex.gakonst.workers.dev/api/auth/chatgpt")!,timeoutInterval:10)
    let began=Date()
    if ws {
     let task=session.webSocketTask(with:request);task.taskDescription=name;task.resume()
     do {_ = try await task.receive(); print("unexpected ready")}catch {print("result \(name) \(Date().timeIntervalSince(began)*1000) rejected")}
     task.cancel(with:.normalClosure,reason:nil)
    } else {
     let _:Int = try await withCheckedThrowingContinuation { continuation in
      let task=session.dataTask(with:request) { _,response,error in
       if let error {continuation.resume(throwing:error)}else{continuation.resume(returning:(response as? HTTPURLResponse)?.statusCode ?? 0)}
      };task.taskDescription=name;task.resume()
     }
     print("result \(name) \(Date().timeIntervalSince(began)*1000) finite")
    }
    session.invalidateAndCancel()
   }
  }
 }
}
