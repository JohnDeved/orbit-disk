import CSQLite
import Foundation

enum InventorySchema {
    // Older inventories may contain cloud storage and cannot be reused under the local-only policy.
    static let version: Int64 = 5
    static let applicationID: Int64 = 0x4F524254

    static let createSQL = """
        CREATE TABLE IF NOT EXISTS metadata (singleton INTEGER PRIMARY KEY CHECK(singleton=1), snapshot_id TEXT NOT NULL,
          root_path TEXT NOT NULL, scanned_at REAL NOT NULL, elapsed REAL NOT NULL, skipped INTEGER NOT NULL,
          issues TEXT NOT NULL, event_id TEXT NOT NULL, event_identity TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, parent INTEGER,
          name TEXT NOT NULL DEFAULT '', search_name TEXT NOT NULL DEFAULT '',
          is_dir INTEGER NOT NULL DEFAULT 0, is_link INTEGER NOT NULL DEFAULT 0, accessible INTEGER NOT NULL DEFAULT 1,
          size INTEGER NOT NULL DEFAULT 0, allocated INTEGER NOT NULL DEFAULT 0, file_count INTEGER NOT NULL DEFAULT 0,
          raw_alloc INTEGER NOT NULL DEFAULT 0, dev INTEGER, inode INTEGER,
          is_mount INTEGER NOT NULL DEFAULT 0, links INTEGER NOT NULL DEFAULT 1, epoch INTEGER NOT NULL DEFAULT 0,
          UNIQUE(parent,name));
        CREATE TABLE IF NOT EXISTS directory_paths (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE);
        CREATE INDEX IF NOT EXISTS children_by_size ON nodes(parent,size DESC,name);
        CREATE INDEX IF NOT EXISTS nodes_by_size ON nodes(size DESC,id,search_name,parent);
        CREATE INDEX IF NOT EXISTS file_identities ON nodes(dev,inode) WHERE is_dir=0 AND dev IS NOT NULL AND inode IS NOT NULL;
        CREATE TABLE IF NOT EXISTS orbit_search_generation (singleton INTEGER PRIMARY KEY CHECK(singleton=1), value INTEGER NOT NULL);
        INSERT OR IGNORE INTO orbit_search_generation VALUES(1,0);
        CREATE TRIGGER IF NOT EXISTS node_search_generation AFTER UPDATE OF search_name ON nodes
          WHEN OLD.search_name<>NEW.search_name BEGIN
          UPDATE orbit_search_generation SET value=value+1 WHERE singleton=1;
        END;
        CREATE TRIGGER IF NOT EXISTS node_directory_path_delete AFTER DELETE ON nodes
          WHEN OLD.is_dir=1 BEGIN DELETE FROM directory_paths WHERE id=OLD.id; END;
        """

    // Directory paths are shared by every child instead of repeated in each file row.
    static func pathSQL(node: String = "n", directory: String = "p") -> String {
        "CASE WHEN \(node).parent IS NULL THEN \(directory).path ELSE \(directory).path || CASE WHEN \(directory).path='/' THEN '' ELSE '/' END || \(node).name END"
    }

    static func identity(_ statement: OpaquePointer, device: Int32, inode: Int32) -> String? {
        guard sqlite3_column_type(statement, device) != SQLITE_NULL,
              sqlite3_column_type(statement, inode) != SQLITE_NULL,
              let deviceValue = Int32(exactly: sqlite3_column_int64(statement, device)) else { return nil }
        return "\(deviceValue):\(UInt64(bitPattern: sqlite3_column_int64(statement, inode)))"
    }

    static func parseIdentity(_ identity: String?) -> (device: Int32, inode: UInt64)? {
        guard let identity else { return nil }
        let parts = identity.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 2, let device = Int32(parts[0]), let inode = UInt64(parts[1]) else { return nil }
        return (device, inode)
    }
}
