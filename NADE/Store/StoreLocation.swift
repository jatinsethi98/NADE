//
//  StoreLocation.swift
//  NADE
//
//  Where the cache lives, and what it is allowed to survive.
//
//  Two files, not one. `docs/PLAN.md` P2 ships a DEBUG fixture world alongside
//  the live client, and "swap the data source" is **not** provenance: both
//  sources would write the same rows, a live launch does not reset (it must
//  keep cached mail for offline), and fixture rows that no live id happened to
//  overwrite would sit beside real mail indefinitely. Two databases make the
//  separation structural instead of hopeful.
//

import Foundation
import GRDB

nonisolated enum StoreLocation: String, Sendable {
    case live = "mail.sqlite"
    case fixtures = "mail-fixtures.sqlite"

    static let directoryName = "NADE"

    var url: URL {
        get throws {
            let base = try FileManager.default.url(
                for: .applicationSupportDirectory, in: .userDomainMask,
                appropriateFor: nil, create: true
            )
            let dir = base.appendingPathComponent(Self.directoryName, isDirectory: true)
            if !FileManager.default.fileExists(atPath: dir.path) {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            }
            return dir.appendingPathComponent(rawValue)
        }
    }

    /// SQLite's three files, in one place.
    ///
    /// A "delete the database" that only removes the first leaves a WAL that
    /// the next open replays — which is how a corrupt store survives the
    /// recovery meant to clear it (EDGE P10) — and protecting only the first
    /// leaves the most recent writes, which live in the WAL, unprotected.
    static func files(for url: URL) -> [URL] {
        [url,
         URL(fileURLWithPath: url.path + "-wal"),
         URL(fileURLWithPath: url.path + "-shm")]
    }

    func removeFiles() throws {
        for file in Self.files(for: try url)
        where FileManager.default.fileExists(atPath: file.path) {
            try FileManager.default.removeItem(at: file)
        }
    }

    /// EDGE (P10): the store is a **cache** of mail the server holds. Erasing
    /// it is always safe, so an unusable file is deleted and retried once
    /// rather than crashing the app on launch.
    ///
    /// `prepare` is the migration. It runs **inside** the retry on purpose: a
    /// database that opens cleanly and then fails to migrate is just as unusable
    /// as one that will not open, and surrounding only the open leaves that file
    /// on disk to fail again on every launch.
    func openWriter(preparing prepare: (any DatabaseWriter) throws -> Void) throws -> any DatabaseWriter {
        do {
            let writer = try Self.makePool(at: try url)
            try prepare(writer)
            return writer
        } catch {
            try removeFiles()
            let writer = try Self.makePool(at: try url)
            try prepare(writer)
            return writer
        }
    }

    private static func makePool(at url: URL) throws -> any DatabaseWriter {
        var configuration = Configuration()
        // Not `.complete`: P3's push-driven sync runs while the device is
        // locked and would take SQLITE_IOERR against a fully protected file.
        // `.completeUntilFirstUserAuthentication` still encrypts at rest before
        // the first unlock after boot.
        configuration.prepareDatabase { db in
            try db.execute(sql: "PRAGMA foreign_keys = ON")
        }
        let pool = try DatabasePool(path: url.path, configuration: configuration)
        try protect(url)
        return pool
    }

    /// One pass over the three files: encrypt at rest until the first unlock,
    /// and keep a cache of the user's server-held mail out of every iCloud
    /// backup and device restore.
    private static func protect(_ url: URL) throws {
        for file in files(for: url) where FileManager.default.fileExists(atPath: file.path) {
            try FileManager.default.setAttributes(
                [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication],
                ofItemAtPath: file.path
            )
            var mutable = file
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try mutable.setResourceValues(values)
        }
    }
}
