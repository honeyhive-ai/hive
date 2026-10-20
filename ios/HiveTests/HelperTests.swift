import Foundation
import Testing
@testable import Hive

@Suite struct MentionTests {
    @Test func activeQuery() {
        #expect(MentionHelper.activeQuery(in: "hi @gp") == "gp")
        #expect(MentionHelper.activeQuery(in: "@") == "")
        #expect(MentionHelper.activeQuery(in: "mail a@b") == nil)
        #expect(MentionHelper.activeQuery(in: "@gpt-oss done") == nil)
        #expect(MentionHelper.activeQuery(in: "plain") == nil)
    }

    @Test func insertCompletesOrAppends() {
        #expect(MentionHelper.insert("gpt-oss", into: "hey @gp") == "hey @gpt-oss ")
        #expect(MentionHelper.insert("gemma", into: "") == "@gemma ")
        #expect(MentionHelper.insert("gemma", into: "look") == "look @gemma ")
        #expect(MentionHelper.insert("gemma", into: "look ") == "look @gemma ")
    }
}

@Suite struct DiffParserTests {
    let diff = """
    diff --git a/src/a.rs b/src/a.rs
    index 1..2 100644
    --- a/src/a.rs
    +++ b/src/a.rs
    @@ -10,4 +10,5 @@ fn main() {
     let a = 1;
    --- removed double-dash line
    +let b = 2;
    +let c = 3;
     let d = 4;
     }
    \\ No newline at end of file
    --- /dev/null
    +++ b/new.txt
    @@ -0,0 +1 @@
    +hello
    """

    @Test func parsesFilesHunksAndNumbers() {
        let files = DiffParser.parse(diff)
        #expect(files.map(\.path) == ["src/a.rs", "new.txt"])
        let a = files[0].lines
        let hunk = a.first { $0.kind == .hunk }
        #expect(hunk != nil)
        let removed = a.filter { $0.kind == .removed }
        #expect(removed.count == 1)
        #expect(removed.first?.text == "-- removed double-dash line")
        #expect(removed.first?.oldNumber == 11)
        let added = a.filter { $0.kind == .added }
        #expect(added.map(\.newNumber) == [11, 12])
        let ctx = a.filter { $0.kind == .context }
        #expect(ctx.map(\.text) == ["let a = 1;", "let d = 4;", "}"])
        #expect(ctx.last?.oldNumber == 13 && ctx.last?.newNumber == 14)
        #expect(files[1].lines.filter { $0.kind == .added }.map(\.text) == ["hello"])
        #expect(DiffParser.stats(diff) == (added: 3, removed: 1))
    }

    @Test func hunkHeaderDefaults() {
        #expect(DiffParser.hunkHeader("@@ -3 +4,2 @@") == .init(oldStart: 3, oldCount: 1, newStart: 4, newCount: 2))
        #expect(DiffParser.hunkHeader("@@ -0,0 +1,10 @@ x -9,9") == .init(oldStart: 0, oldCount: 0, newStart: 1, newCount: 10))
    }

    @Test func bareHunkWithoutHeaders() {
        let files = DiffParser.parse("@@ -1,2 +1,2 @@\n-a\n+b\n c\n")
        #expect(files.count == 1)
        #expect(files[0].lines.map(\.kind) == [.hunk, .removed, .added, .context])
    }

    @Test func fixtureDiffsAreWellFormed() {
        for p in MockFixtures.make(now: Date()).proposals {
            let files = DiffParser.parse(p.diff ?? "")
            #expect(!files.isEmpty)
            #expect(files.allSatisfy { !$0.lines.contains { $0.kind == .meta && $0.text.hasPrefix("+") } })
        }
    }
}

@Suite struct MarkdownTests {
    @Test func blocks() {
        let md = """
        # Title
        Some **bold** text
        continues here.

        - one
        - two
        1. first

        > quoted

        ```swift
        let x = 1
        ```
        ---
        ```
        open fence
        """
        let blocks = MarkdownBlock.parse(md)
        #expect(blocks == [
            .heading(level: 1, text: "Title"),
            .paragraph("Some **bold** text\ncontinues here."),
            .listItem(marker: "•", text: "one", indent: 0),
            .listItem(marker: "•", text: "two", indent: 0),
            .listItem(marker: "1.", text: "first", indent: 0),
            .quote("quoted"),
            .code(language: "swift", text: "let x = 1", closed: true),
            .rule,
            .code(language: nil, text: "open fence", closed: false),
        ])
    }

    @Test func inlineCodeAndBold() {
        let s = inlineMarkdown("use `cargo` **now**")
        #expect(String(s.characters) == "use cargo now")
    }
}
