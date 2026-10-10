// Standalone macOS SwiftData application control. No SwiftData engine dependency.
// Compile with the identified Xcode SDK; create and verify in separate processes.
import Foundation
import SwiftData

@Model final class Character {
  #Index<Character>([\.x, \.id])
  @Attribute(.unique) var id: Int64
  var name: String
  var x: Int32
  var y: Int32
  var health: Float
  var alive: Bool
  var team: UInt64?
  init(id: Int64, x: Int32, team: UInt64?) {
    self.id = id
    self.name = "player-\(id)"
    self.x = x
    self.y = -8
    self.health = 100
    self.alive = true
    self.team = team
  }
}
@Model final class Item {
  #Index<Item>([\.owner, \.kind, \.id])
  @Attribute(.unique) var id: Int64
  var owner: Int64
  var kind: Int64
  init(id: Int64, owner: Int64, kind: Int64) {
    self.id = id
    self.owner = owner
    self.kind = kind
  }
}
enum Rejected: Error { case edit }
@main struct Journey {
  @MainActor static func player(_ context: ModelContext, _ id: Int64) throws -> Character {
    try context.fetch(FetchDescriptor<Character>(predicate: #Predicate { $0.id == id })).first!
  }
  @MainActor static func item(_ context: ModelContext, _ id: Int64) throws -> Item {
    try context.fetch(FetchDescriptor<Item>(predicate: #Predicate { $0.id == id })).first!
  }
  @MainActor static func visible(_ context: ModelContext) throws -> [Character] {
    let lower: Int32 = -64
    let upper: Int32 = 64
    let yMin: Int32 = -16
    var query = FetchDescriptor<Character>(
      predicate: #Predicate {
        $0.x >= lower && $0.x < upper && $0.y >= yMin && $0.alive && $0.health > 0
      }, sortBy: [SortDescriptor(\.x), SortDescriptor(\.id)])
    query.fetchLimit = 8
    return try context.fetch(query)
  }
  @MainActor static func inventory(_ context: ModelContext, owner: Int64, kind: Int64) throws -> [(
    Item, Character
  )] {
    var query = FetchDescriptor<Item>(
      predicate: #Predicate { $0.owner == owner && $0.kind == kind }, sortBy: [SortDescriptor(\.id)]
    )
    query.fetchLimit = 4
    return try context.fetch(query).map { it in (it, try player(context, it.owner)) }
  }
  @MainActor static func verify(_ context: ModelContext) throws {
    let characterCount = try context.fetchCount(FetchDescriptor<Character>())
    let itemCount = try context.fetchCount(FetchDescriptor<Item>())
    precondition(characterCount == 2 && itemCount == 2)
    let first = try player(context, 7)
    let second = try player(context, 9)
    precondition(
      first.name == "player-7" && first.x == 128 && first.y == -8 && first.health == 0
        && !first.alive && first.team == nil)
    precondition(
      second.name == "player-9" && second.x == 16 && second.y == -8 && second.health == 100
        && second.alive && second.team == 42)
    let moved = try item(context, 101)
    let untouched = try item(context, 103)
    precondition(moved.owner == 9 && moved.kind == 1 && untouched.owner == 7 && untouched.kind == 2)
    let selected = try visible(context)
    precondition(selected.map(\.id) == [9])
    let joined = try inventory(context, owner: 9, kind: 1)
    precondition(
      joined.count == 1 && joined[0].0.id == 101 && joined[0].0.owner == 9 && joined[0].0.kind == 1
        && joined[0].1.id == 9 && joined[0].1.name == "player-9" && joined[0].1.x == 16
        && joined[0].1.y == -8 && joined[0].1.health == 100 && joined[0].1.alive
        && joined[0].1.team == 42)
  }
  @MainActor static func main() throws {
    let args = CommandLine.arguments
    precondition(args.count == 3 && ["create", "verify"].contains(args[1]))
    let url = URL(fileURLWithPath: args[2])
    if args[1] == "create" { precondition(!FileManager.default.fileExists(atPath: url.path)) }
    let schema = Schema([Character.self, Item.self])
    let configuration = ModelConfiguration(
      "GameJourney", schema: schema, url: url, cloudKitDatabase: .none)
    let container = try ModelContainer(for: schema, configurations: [configuration])
    let context = ModelContext(container)
    context.autosaveEnabled = false
    if args[1] == "create" {
      context.insert(Character(id: 7, x: -32, team: nil))
      context.insert(Character(id: 9, x: 16, team: 42))
      context.insert(Item(id: 101, owner: 7, kind: 1))
      context.insert(Item(id: 103, owner: 7, kind: 2))
      try context.save()
      let initial = try visible(context)
      precondition(initial.map(\.id) == [7, 9])
      let joined = try inventory(context, owner: 7, kind: 1)
      precondition(
        joined.count == 1 && joined[0].0.id == 101 && joined[0].0.owner == 7
          && joined[0].0.kind == 1 && joined[0].1.id == 7 && joined[0].1.name == "player-7"
          && joined[0].1.x == -32 && joined[0].1.y == -8 && joined[0].1.health == 100
          && joined[0].1.alive && joined[0].1.team == nil)
      let first = try player(context, 7)
      let moved = try item(context, 101)
      try context.transaction {
        first.x = 128
        first.health = 0
        first.alive = false
        moved.owner = 9
      }
      try verify(ModelContext(container))
      do {
        let second = try player(context, 9)
        let untouched = try item(context, 103)
        try context.transaction {
          second.health = 0
          second.alive = false
          untouched.owner = 9
          throw Rejected.edit
        }
        preconditionFailure("expected rejection")
      } catch Rejected.edit {
        print("throwing_transaction_has_changes=\(context.hasChanges)")
        context.rollback()
      }
    }
    try verify(context)
    print(
      "swiftdata,phase=\(args[1]),full_model_and_join=verified,atomic_edit=verified,error_with_explicit_rollback=verified"
    )
  }
}
