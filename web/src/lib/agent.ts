// The workspace's default agent — the one that answers untagged messages (in a
// solo workspace) and any message tagged @hive or @primary. It is synthesized
// by the backend at dispatch time (no roster record), so the UI names it from
// these constants wherever it must appear: the turn head, the route pill, the
// mention list, and the roster/count.
//
//   PRIMARY_NAME   — display name + avatar initials ("Hive")
//   PRIMARY_HANDLE — mentionable handle, rendered "@hive"
//
// "@primary" stays a reserved router alias (a role, not a name); both resolve to
// this same default agent.
export const PRIMARY_NAME = "Hive";
export const PRIMARY_HANDLE = "hive";
