import Foundation

/// Fixture data for mock mode and tests.
enum MockFixtures {
    static let labChatId = "c-lab"

    struct Bundle {
        var chats: [Chat]
        var messages: [String: [Message]]
        var agents: [Agent]
        var hosts: [Host]
        var proposals: [Proposal]
    }

    static func make(now: Date) -> Bundle {
        func ago(_ s: Double) -> Date { now.addingTimeInterval(-s) }

        let hosts = [
            Host(id: "h-sff", label: "sff", isThis: true, online: true),
            Host(id: "h-mac", label: "macbook", isThis: false, online: true),
            Host(id: "h-laptop", label: "laptop", isThis: false, online: false),
        ]
        let agents = [
            Agent(id: "a-gptoss", name: "gpt-oss", runtimeLabel: "Local (gateway)", model: "gpt-oss:20b",
                  hostId: "h-sff", hostLabel: "sff"),
            Agent(id: "a-gemma", name: "gemma", runtimeLabel: "Local (gateway)", model: "gemma4:12b",
                  hostId: "h-sff", hostLabel: "sff"),
            Agent(id: "a-claude", name: "claude", runtimeLabel: "Claude Code", model: nil,
                  hostId: "h-mac", hostLabel: "macbook"),
            Agent(id: "a-lfm", name: "lfm", runtimeLabel: "Local (gateway)", model: "lfm2:8b",
                  hostId: nil, hostLabel: nil),
        ]

        let lab: [Message] = [
            Message(id: "m1", chatId: labChatId, author: "you", role: .user,
                    body: "@gpt-oss can you check why the rdnamcp tool calls were slow last night?",
                    createdAt: ago(3600)),
            Message(id: "m2", chatId: labChatId, author: "gpt-oss", role: .agent, agentId: "a-gptoss",
                    body: """
                    Looked at the gateway logs. The slow calls line up with **prompt re-evaluation** after the \
                    KV cache was evicted:

                    - 03:12 to 03:40: cache hit rate dropped to *12%*
                    - every turn re-read ~18k tokens of history

                    ### Suggested fix
                    Raise the slot cache and pin the system prompt:

                    ```toml
                    [models."gpt-oss:20b"]
                    ctx_size = 32768
                    cache_reuse = 256
                    slot_save_path = "/var/lib/llama/slots"
                    ```

                    Then restart with `systemctl --user restart llama-gateway`. See the \
                    [llama.cpp server docs](https://github.com/ggml-org/llama.cpp) for `cache_reuse`.
                    """,
                    createdAt: ago(3500)),
            Message(id: "m3", chatId: labChatId, author: "you", role: .user,
                    body: "Sounds good. Open a proposal and run the smoke tests first.",
                    createdAt: ago(600)),
        ]
        let infra: [Message] = [
            Message(id: "m10", chatId: "c-infra", author: "you", role: .user,
                    body: "@gemma summarize power draw for the week", createdAt: ago(7200 * 3)),
            Message(id: "m11", chatId: "c-infra", author: "gemma", role: .agent, agentId: "a-gemma",
                    body: "Average **214 W** across both boxes, peaking at 412 W during the Tuesday eval run.",
                    createdAt: ago(7200 * 3 - 30)),
        ]
        let notes: [Message] = [
            Message(id: "m20", chatId: "c-notes", author: "system", role: .system,
                    body: "claude moved to host macbook", createdAt: ago(86400 * 2)),
        ]

        let chats = [
            Chat(id: labChatId, title: "rdnamcp latency", updatedAt: ago(600),
                 preview: "Sounds good. Open a proposal and run the smoke tests first.", unread: 1),
            Chat(id: "c-infra", title: "Power report", updatedAt: ago(7200 * 3 - 30),
                 preview: "Average 214 W across both boxes, peaking at 412 W…", unread: 0),
            Chat(id: "c-notes", title: "Agent hosts", updatedAt: ago(86400 * 2),
                 preview: "claude moved to host macbook", unread: 0),
            Chat(id: "c-ideas", title: "Weekend ideas", updatedAt: ago(86400 * 6), preview: "", unread: 3),
        ]

        let proposals = [
            Proposal(id: "p1", chatId: labChatId, title: "Raise KV cache reuse for gpt-oss:20b",
                     body: "Keeps long agent loops from re-evaluating the full prompt after cache eviction.",
                     kind: "edit", diff: """
                     diff --git a/gateway/models.toml b/gateway/models.toml
                     index 3f1c2a0..9b7d4e1 100644
                     --- a/gateway/models.toml
                     +++ b/gateway/models.toml
                     @@ -12,8 +12,9 @@ backend = "llama.cpp"
                      [models."gpt-oss:20b"]
                      path = "/models/gpt-oss-20b-mxfp4.gguf"
                     -ctx_size = 16384
                     +ctx_size = 32768
                      n_gpu_layers = 99
                     -cache_reuse = 0
                     +cache_reuse = 256
                     +slot_save_path = "/var/lib/llama/slots"
                      reasoning_effort = "low"
                      parallel = 2

                     @@ -40,3 +42,4 @@ parallel = 4
                      [models."lfm2:8b"]
                      path = "/models/lfm2-8b-q4_k_m.gguf"
                      ctx_size = 8192
                     +flash_attn = true
                     """, status: .open, createdAt: ago(300), author: "@gpt-oss"),
            Proposal(id: "p2", chatId: "c-infra", title: "Add power alert to ntfy",
                     body: "Alert when lab:power_watts:sum stays above 450 W for 10 minutes.",
                     kind: "edit", diff: """
                     --- a/monitoring/rules/power.yml
                     +++ b/monitoring/rules/power.yml
                     @@ -1,4 +1,12 @@
                      groups:
                        - name: power
                          rules:
                     +      - alert: LabPowerHigh
                     +        expr: lab:power_watts:sum > 450
                     +        for: 10m
                     +        labels:
                     +          severity: warning
                     +        annotations:
                     +          summary: "Lab drawing {{ $value }} W"
                     +
                            - record: lab:power_watts:sum
                     """, status: .open, createdAt: ago(5400), author: "@gemma"),
        ]

        return Bundle(chats: chats,
                      messages: [labChatId: lab, "c-infra": infra, "c-notes": notes, "c-ideas": []],
                      agents: agents, hosts: hosts, proposals: proposals)
    }

    static func lab(now: Date) -> LabStatus {
        LabStatus(
            workers: [
                LabWorker(name: "desktop-6900xt", up: true),
                LabWorker(name: "sff-6700xt", up: true),
                LabWorker(name: "jetson", up: false),
            ],
            powerWatts: 236.4, costPerHour: 0.0371, ratePerKwh: 0.157,
            temps: [
                LabTemp(name: "6900 XT junction", celsius: 71.0),
                LabTemp(name: "6700 XT edge", celsius: 58.5),
                LabTemp(name: "SFF CPU", celsius: 49.2),
                LabTemp(name: "Jetson SoC", celsius: 41.0),
            ],
            solar: solar(now: now),
            updatedAt: now)
    }

    /// A sunny afternoon: exporting, with the panels covering the lab.
    static func solar(now: Date) -> LabSolar {
        let hour = 3600.0
        let start = now.addingTimeInterval(-24 * hour)
        let history = (0...24).map { i -> SolarPoint in
            let t = start.addingTimeInterval(Double(i) * hour)
            let h = Calendar.current.component(.hour, from: t)
            let sun = max(0, sin(Double(h - 6) / 13 * .pi)) * 5200
            return SolarPoint(t: t, solarWatts: sun.rounded(), labWatts: 180 + Double(i % 5) * 9)
        }
        return LabSolar(up: true, productionWatts: 4310, consumptionWatts: 1180, gridWatts: -3130,
                        houseSolarShare: 1, labSolarWatts: 236.4, producedKwh24h: 31.6, houseKwh24h: 22.4,
                        labKwh24h: 4.71, labSolarKwh24h: 2.38, gridImportKwh24h: 9.8, gridExportKwh24h: 19.1,
                        labOffset24h: 0.505, labSolarValue24h: 0.374, history: history)
    }
}
