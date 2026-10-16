// Interactive kinds that only print or follow output; the dashboard streams them into a sheet.
// The other interactive kinds (shells, sudo, prompts, `host forward`) open Terminal.app.
export const STREAMED = new Set(["ContainerLogs", "AppLogs", "JobLogs", "SandboxDiff", "StorageEnv"])

/** Shown in red and kept behind the row's menu. */
export const DESTRUCTIVE = new Set([
  "ContainerRemove",
  "SnapRemove",
  "SandboxRemove",
  "AppDestroy",
  "JobCancel",
  "VmStop",
  "ProjectDown",
  "ImageRemove",
  "VolumeRemove",
  "ImagePrune",
  "VolumePrune",
  "BuildCachePrune",
])
