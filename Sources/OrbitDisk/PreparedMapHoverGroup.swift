import Foundation

struct PreparedMapHoverGroup {
    let id: String
    let parentID: String
    let rootID: String
    let mapRevision: UUID
    let memberIDs: Set<String>
}
