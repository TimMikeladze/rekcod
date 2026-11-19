// Interactive kinds that only print or follow output; the dashboard streams them into a sheet.
// The other interactive kinds (shells, sudo, prompts, `host forward`) open Terminal.app, and so
// do Reset and ResetAll: a stream ends with its sheet, and a reset must not stop half way.
export const STREAMED = new Set(["ContainerLogs", "AppLogs", "JobLogs", "SandboxDiff", "StorageEnv", "Doctor", "ImageHistory", "VolumeBrowse"])

/**
 * Shown in red and kept behind the row's menu: the actions that ask first. `rekcod state` sets
 * `confirm` on exactly the ones that destroy or overwrite something
 * (actions::tests::destructive_actions_ask_first), so there's no second list to keep in step.
 */
export const destructive = (a: { confirm?: string | null }) => a.confirm != null
