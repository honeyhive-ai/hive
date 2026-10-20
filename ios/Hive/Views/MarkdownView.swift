import SwiftUI
import UIKit

/// Block-level Markdown split; inline styling is left to `AttributedString`.
enum MarkdownBlock: Equatable, Hashable {
    case paragraph(String)
    case heading(level: Int, text: String)
    case listItem(marker: String, text: String, indent: Int)
    case quote(String)
    case code(language: String?, text: String, closed: Bool)
    case rule

    static func parse(_ source: String) -> [MarkdownBlock] {
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []
        var code: (lang: String?, lines: [String], fence: String)?

        func flush() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.joined(separator: "\n")))
                paragraph.removeAll()
            }
        }

        for rawLine in source.components(separatedBy: "\n") {
            let line = rawLine.hasSuffix("\r") ? String(rawLine.dropLast()) : rawLine
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if var open = code {
                if trimmed.hasPrefix(open.fence) && trimmed.allSatisfy({ String($0) == String(open.fence.first!) }) {
                    blocks.append(.code(language: open.lang, text: open.lines.joined(separator: "\n"), closed: true))
                    code = nil
                } else {
                    open.lines.append(line)
                    code = open
                }
                continue
            }

            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                flush()
                let fenceChar = trimmed.first!
                let fence = String(trimmed.prefix { $0 == fenceChar })
                let lang = trimmed.dropFirst(fence.count).trimmingCharacters(in: .whitespaces)
                code = (lang.isEmpty ? nil : lang, [], fence)
                continue
            }
            if trimmed.isEmpty { flush(); continue }

            if let h = heading(trimmed) { flush(); blocks.append(h); continue }
            if trimmed == "---" || trimmed == "***" || trimmed == "___" { flush(); blocks.append(.rule); continue }
            if trimmed.hasPrefix(">") {
                flush()
                let text = trimmed.dropFirst().trimmingCharacters(in: .whitespaces)
                if case let .quote(prev)? = blocks.last {
                    blocks[blocks.count - 1] = .quote(prev + "\n" + text)
                } else {
                    blocks.append(.quote(text))
                }
                continue
            }
            if let item = listItem(line) { flush(); blocks.append(item); continue }
            paragraph.append(trimmed)
        }
        flush()
        if let open = code {
            blocks.append(.code(language: open.lang, text: open.lines.joined(separator: "\n"), closed: false))
        }
        return blocks
    }

    private static func heading(_ t: String) -> MarkdownBlock? {
        let hashes = t.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), t.count > hashes,
              t[t.index(t.startIndex, offsetBy: hashes)] == " " else { return nil }
        return .heading(level: hashes, text: String(t.dropFirst(hashes + 1)))
    }

    private static func listItem(_ line: String) -> MarkdownBlock? {
        let indent = line.prefix { $0 == " " }.count / 2
        let t = line.trimmingCharacters(in: .whitespaces)
        for bullet in ["- ", "* ", "+ "] where t.hasPrefix(bullet) {
            return .listItem(marker: "•", text: String(t.dropFirst(2)), indent: indent)
        }
        let digits = t.prefix { $0.isNumber }
        if !digits.isEmpty, digits.count <= 3 {
            let rest = t.dropFirst(digits.count)
            if rest.hasPrefix(". ") || rest.hasPrefix(") ") {
                return .listItem(marker: "\(digits).", text: String(rest.dropFirst(2)), indent: indent)
            }
        }
        return nil
    }
}

/// Renders an inline-Markdown string (bold, italics, code, links).
func inlineMarkdown(_ s: String) -> AttributedString {
    let opts = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace,
                                                       failurePolicy: .returnPartiallyParsedIfPossible)
    var out = (try? AttributedString(markdown: s, options: opts)) ?? AttributedString(s)
    for run in out.runs where run.inlinePresentationIntent?.contains(.code) == true {
        out[run.range].font = .system(.callout, design: .monospaced)
        out[run.range].backgroundColor = Color.white.opacity(0.08)
    }
    return out
}

struct MarkdownView: View {
    let text: String
    var streaming = false

    var body: some View {
        let blocks = MarkdownBlock.parse(text)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
    }

    @ViewBuilder
    private func view(for block: MarkdownBlock) -> some View {
        switch block {
        case let .paragraph(s):
            Text(inlineMarkdown(s)).fixedSize(horizontal: false, vertical: true)
        case let .heading(level, s):
            Text(inlineMarkdown(s))
                .font(level <= 1 ? .title3.bold() : level == 2 ? .headline : .subheadline.bold())
                .padding(.top, 2)
        case let .listItem(marker, s, indent):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(marker).foregroundStyle(.secondary).monospacedDigit()
                Text(inlineMarkdown(s)).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, CGFloat(indent) * 14)
        case let .quote(s):
            HStack(spacing: 8) {
                RoundedRectangle(cornerRadius: 1.5).fill(.secondary).frame(width: 3)
                Text(inlineMarkdown(s)).foregroundStyle(.secondary)
            }
        case let .code(lang, s, _):
            CodeBlockView(language: lang, code: s)
        case .rule:
            Divider()
        }
    }
}

struct CodeBlockView: View {
    let language: String?
    let code: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language ?? "code").font(.caption2.monospaced()).foregroundStyle(.secondary)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .font(.caption2)
                        .labelStyle(.titleAndIcon)
                }
                .buttonStyle(.plain)
                .foregroundStyle(copied ? .green : .secondary)
                .accessibilityLabel("Copy code")
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Color.white.opacity(0.04))
            ScrollView(.horizontal, showsIndicators: true) {
                Text(code)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(10)
            }
        }
        .background(Theme.codeBackground, in: .rect(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.white.opacity(0.08)))
    }
}
