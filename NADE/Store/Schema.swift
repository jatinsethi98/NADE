//
//  Schema.swift
//  NADE
//
//  The one migration, and the reasoning behind the columns that are not
//  obvious. `docs/PLAN.md` §iOS app: "GRDB mirrors the wire: `thread` per
//  contract; `thread_mailbox` join; cursors in `mailbox_sync`."
//
//  **Mirroring the wire means mirroring its nullability.** `docs/API.md` §0:
//  "A field with no `|null` is always present and never null." A column that
//  allows null where the contract does not is a row the non-optional records
//  cannot decode — and a decode failure inside a `ValueObservation` does not
//  lose one row, it ends the observation and blanks the screen.
//

import Foundation
import GRDB

nonisolated enum Schema {

    static func migrator() -> DatabaseMigrator {
        var migrator = DatabaseMigrator()
        // Deliberately NOT `eraseDatabaseOnSchemaChange`, even in DEBUG: it
        // turns a migration mistake into a silent wipe, which is precisely the
        // failure a migration test is supposed to catch.
        migrator.registerMigration("v1-mail") { db in
            try db.create(table: "account") { t in
                // One account in v1. The check is what makes it a singleton —
                // a second row is a constraint failure, not a surprise later.
                t.column("id", .integer).primaryKey().check { $0 == 1 }
                t.column("email", .text).notNull()
                // No CHECK on status. See `WireEnum`: an unknown value from a
                // future server decodes to `.unknown(raw)` so one new case
                // cannot blank a screen — and a CHECK pinned to today's values
                // would just move that failure from the decoder to the write.
                t.column("status", .text).notNull()
            }

            try db.create(table: "mailbox") { t in
                t.column("id", .text).primaryKey()
                t.column("name", .text).notNull()
                t.column("kind", .text).notNull()
                // Threads, not messages, and within the synced window.
                t.column("unread", .integer).notNull()
                t.column("total", .integer).notNull()
                // `position` is DATA, not a derivable property. The wire order
                // is eight system labels in a *semantic* order (INBOX … SENT)
                // and then user labels by **raw byte value of the name**.
                // Neither is reproducible with an ORDER BY over these columns,
                // and API.md chose byte order precisely so the two sides cannot
                // disagree on a device with a different collation.
                t.column("position", .integer).notNull()
            }

            try db.create(table: "thread") { t in
                t.column("id", .text).primaryKey()
                // The list half — every one non-null on the wire.
                t.column("subject", .text).notNull()
                t.column("snippet", .text).notNull()
                t.column("from_name", .text).notNull()
                t.column("from_email", .text).notNull()
                t.column("ts", .text).notNull()
                t.column("unread", .boolean).notNull()
                // The server's count. Never `messages.count`; `thread_partial`
                // is the fixture where the two differ on purpose.
                t.column("msg_count", .integer).notNull()
                t.column("agent_note", .text)

                // The detail half. Null until `GET /threads/{id}` has landed,
                // which is what `detail_fetched_at IS NULL` means. They are
                // columns on `thread` rather than a second table so that
                // opening a thread cannot contradict the row behind it.
                //
                // They are also why every write in `MailStore` is
                // **column-scoped**: a whole-record upsert built from a
                // `WireThreadRow` would nil these out and make an opened thread
                // "never loaded" again on the next list refresh.
                t.column("detail_mailbox_name", .text)
                t.column("detail_account_email", .text)
                t.column("detail_partial", .boolean)
                t.column("detail_fetched_at", .text)

                // There is deliberately **no local read-marker column.**
                // API.md §2: v1 holds `gmail.readonly`, so opening a thread
                // cannot clear Gmail's UNREAD label, and a local flag would
                // disagree with Gmail within minutes — "the exact failure this
                // app exists to avoid". Having no column is what makes that
                // unwritable rather than merely discouraged.
            }
            try db.create(index: "thread_by_ts", on: "thread", columns: ["ts", "id"])

            try db.create(table: "thread_mailbox") { t in
                t.column("mailbox_id", .text).notNull()
                    .references("mailbox", onDelete: .cascade)
                t.column("thread_id", .text).notNull()
                    .references("thread", onDelete: .cascade)
                t.primaryKey(["mailbox_id", "thread_id"])
            }

            try db.create(table: "message") { t in
                // API.md: "There is no `id` field on a message." `gmail_id` is
                // the identity; the server's bigint never leaves its database.
                t.column("gmail_id", .text).primaryKey()
                t.column("thread_id", .text).notNull()
                    .references("thread", onDelete: .cascade)
                // Oldest first, as the wire delivers them.
                t.column("position", .integer).notNull()
                t.column("from_name", .text).notNull()
                t.column("from_email", .text).notNull()
                t.column("ts", .text).notNull()
                // Never null; may be "". Synthesised from HTML for the 47% of
                // real mail with no text/plain part.
                t.column("body_text", .text).notNull()
                // Null when there is genuinely no text/html part. NOT a
                // rendering of the plain text — P3's "View original" keys off
                // exactly this, which is why P2 stores it without showing it.
                t.column("body_html", .text)
                // Leaf arrays, always read whole and never queried or joined.
                t.column("to_json", .text).notNull()
                t.column("cc_json", .text).notNull()
                t.column("label_ids_json", .text).notNull()
                t.column("attachments_json", .text).notNull()
            }

            try db.create(table: "agent_card") { t in
                t.column("run_id", .text).primaryKey()
                t.column("thread_id", .text).notNull()
                    .references("thread", onDelete: .cascade)
                // Newest first, as the wire delivers them.
                t.column("position", .integer).notNull()
                t.column("agent_name", .text).notNull()
                t.column("status", .text).notNull()
                t.column("summary", .text).notNull()
                t.column("feed_item_id", .text)
            }

            try db.create(table: "mailbox_sync") { t in
                t.column("mailbox_id", .text).primaryKey()
                    .references("mailbox", onDelete: .cascade)
                t.column("next_cursor", .text)
                // `next_cursor IS NULL` conflates "never fetched" with "reached
                // the last page". Two states need two bits, and the list's
                // load-more depends on telling them apart (EDGE P5).
                t.column("reached_end", .boolean).notNull().defaults(to: false)
                // Bumped whenever the mailbox is refreshed from the top. A page
                // that was in flight across that refresh carries the old
                // generation and is discarded rather than stitched onto a list
                // it no longer belongs to.
                t.column("generation", .integer).notNull().defaults(to: 0)
                t.column("last_page_at", .text)
            }
        }
        return migrator
    }
}
