import SwiftUI

struct DiffLine: Equatable, Hashable, Sendable {
    enum Kind: Equatable, Hashable, Sendable { case fileHeader, hunk, added, removed, context, meta }
    var kind: Kind
    var text: String
    var oldNumber: Int?
    var newNumber: Int?
}

struct DiffFile: Equatable, Sendable {
    var path: String
    var lines: [DiffLine]
}

/// Unified-diff parser with old/new line numbering.
enum DiffParser {
    static func parse(_ diff: String) -> [DiffFile] {
        var files: [DiffFile] = []
        var current: DiffFile?
        var oldLine = 0
        var newLine = 0
        // Lines still expected in the current hunk (from its header counts).
        var oldLeft = 0
        var newLeft = 0
        var inHunk: Bool { oldLeft > 0 || newLeft > 0 }

        func startFile(_ path: String) {
            if let current { files.append(current) }
            current = DiffFile(path: path, lines: [])
            oldLeft = 0
            newLeft = 0
        }

        for raw in diff.components(separatedBy: "\n") {
            let line = raw.hasSuffix("\r") ? String(raw.dropLast()) : raw

            if inHunk {
                if line.hasPrefix("+") {
                    current?.lines.append(DiffLine(kind: .added, text: String(line.dropFirst()), newNumber: newLine))
                    newLine += 1
                    newLeft -= 1
                } else if line.hasPrefix("-") {
                    current?.lines.append(DiffLine(kind: .removed, text: String(line.dropFirst()), oldNumber: oldLine))
                    oldLine += 1
                    oldLeft -= 1
                } else if line.hasPrefix("\\") {
                    current?.lines.append(DiffLine(kind: .meta, text: line))
                } else {
                    // Context (leading space); tolerate blank lines whose space was trimmed.
                    let text = line.hasPrefix(" ") ? String(line.dropFirst()) : line
                    current?.lines.append(DiffLine(kind: .context, text: text, oldNumber: oldLine, newNumber: newLine))
                    oldLine += 1
                    newLine += 1
                    oldLeft -= 1
                    newLeft -= 1
                }
                continue
            }
            if line.hasPrefix("\\") {
                current?.lines.append(DiffLine(kind: .meta, text: line))
                continue
            }

            if line.hasPrefix("diff --git ") {
                let last = line.split(separator: " ").last.map(String.init) ?? "file"
                startFile(last.hasPrefix("b/") ? String(last.dropFirst(2)) : last)
                continue
            }
            if line.hasPrefix("--- ") {
                let p = path(from: line)
                let hasHunks = current?.lines.contains { $0.kind == .hunk } ?? false
                if current == nil || hasHunks {
                    startFile(p)
                } else if p != "/dev/null" {
                    current?.path = p
                }
                continue
            }
            if line.hasPrefix("+++ ") {
                let p = path(from: line)
                if current == nil { startFile(p) } else if p != "/dev/null" { current?.path = p }
                continue
            }
            if line.hasPrefix("@@") {
                if current == nil { startFile("file") }
                let h = hunkHeader(line)
                oldLine = h.oldStart
                newLine = h.newStart
                oldLeft = h.oldCount
                newLeft = h.newCount
                current?.lines.append(DiffLine(kind: .hunk, text: line))
                continue
            }
            if line.isEmpty { continue }
            if current == nil { startFile("file") }
            current?.lines.append(DiffLine(kind: .meta, text: line))
        }
        if let current { files.append(current) }
        return files
    }

    static func stats(_ diff: String) -> (added: Int, removed: Int) {
        var a = 0, r = 0
        for f in parse(diff) {
            for l in f.lines {
                if l.kind == .added { a += 1 } else if l.kind == .removed { r += 1 }
            }
        }
        return (a, r)
    }

    private static func path(from header: String) -> String {
        var p = String(header.dropFirst(4))
        if let tab = p.firstIndex(of: "\t") { p = String(p[..<tab]) }
        if p.hasPrefix("a/") || p.hasPrefix("b/") { p = String(p.dropFirst(2)) }
        return p
    }

    struct HunkHeader: Equatable {
        var oldStart: Int, oldCount: Int, newStart: Int, newCount: Int
    }

    /// `@@ -12,9 +12,11 @@ ctx` -> (12, 9, 12, 11); a missing count means 1.
    static func hunkHeader(_ line: String) -> HunkHeader {
        var h = HunkHeader(oldStart: 1, oldCount: 1, newStart: 1, newCount: 1)
        let ranges = line.split(separator: "@@", omittingEmptySubsequences: true).first ?? ""
        for token in ranges.split(separator: " ") {
            guard let sign = token.first, sign == "-" || sign == "+" else { continue }
            let nums = token.dropFirst().split(separator: ",").map { Int($0) }
            let start = nums.first.flatMap { $0 } ?? 1
            let count = nums.count > 1 ? (nums[1] ?? 1) : 1
            if sign == "-" { h.oldStart = start; h.oldCount = count } else { h.newStart = start; h.newCount = count }
        }
        return h
    }
}

struct DiffView: View {
    let diff: String

    var body: some View {
        let files = DiffParser.parse(diff)
        VStack(alignment: .leading, spacing: 14) {
            ForEach(Array(files.enumerated()), id: \.offset) { _, file in
                VStack(alignment: .leading, spacing: 0) {
                    HStack {
                        Image(systemName: "doc.text").foregroundStyle(.secondary)
                        Text(file.path).font(.footnote.monospaced().weight(.semibold)).lineLimit(1)
                            .truncationMode(.head)
                        Spacer()
                        let a = file.lines.filter { $0.kind == .added }.count
                        let r = file.lines.filter { $0.kind == .removed }.count
                        Text("+\(a)").foregroundStyle(Theme.added).font(.caption.monospacedDigit())
                        Text("−\(r)").foregroundStyle(Theme.removed).font(.caption.monospacedDigit())
                    }
                    .padding(.horizontal, 10).padding(.vertical, 8)
                    .background(Color.white.opacity(0.05))
                    ScrollView(.horizontal) {
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(file.lines.enumerated()), id: \.offset) { _, line in
                                DiffLineRow(line: line)
                            }
                        }
                        .padding(.vertical, 4)
                    }
                }
                .background(Theme.codeBackground, in: .rect(cornerRadius: 12))
                .clipShape(.rect(cornerRadius: 12))
                .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.white.opacity(0.08)))
            }
        }
    }
}

struct DiffLineRow: View {
    let line: DiffLine

    var body: some View {
        HStack(spacing: 0) {
            if line.kind == .hunk || line.kind == .meta {
                Text(line.text)
                    .foregroundStyle(line.kind == .hunk ? Theme.hunk : .secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
            } else {
                Text(line.oldNumber.map(String.init) ?? "").frame(width: 34, alignment: .trailing)
                    .foregroundStyle(.tertiary)
                Text(line.newNumber.map(String.init) ?? "").frame(width: 34, alignment: .trailing)
                    .foregroundStyle(.tertiary)
                Text(marker).foregroundStyle(color).frame(width: 18)
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(line.kind == .context ? .primary : color)
                    .padding(.trailing, 12)
            }
        }
        .font(.system(.caption, design: .monospaced))
        .fixedSize()
        .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
        .background(background)
    }

    private var marker: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        default: " "
        }
    }

    private var color: Color {
        switch line.kind {
        case .added: Theme.added
        case .removed: Theme.removed
        default: .secondary
        }
    }

    private var background: Color {
        switch line.kind {
        case .added: Theme.added.opacity(0.13)
        case .removed: Theme.removed.opacity(0.13)
        case .hunk: Theme.hunk.opacity(0.10)
        default: .clear
        }
    }
}
