//
//  AttachmentSize.swift
//  NADE
//
//  The 11 pt figure beside an attachment tag on 1f.
//
//  `.binary`, not `.file`. The mockup prints **240 KB** for the 245 760-byte
//  role outline, which is 245 760 / 1024. `.file` divides by 1000 and prints
//  246 KB — a one-digit difference that is invisible until it is beside the
//  render it is supposed to match.
//

import Foundation

nonisolated enum AttachmentSize {
    static func string(bytes: Int) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .binary
        formatter.allowedUnits = [.useKB, .useMB, .useGB]
        formatter.includesActualByteCount = false
        return formatter.string(fromByteCount: Int64(bytes))
    }
}
