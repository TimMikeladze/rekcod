import type { MetadataRoute } from "next"

export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "rekcod",
    short_name: "rekcod",
    description: "Containers, apps, jobs and sandboxes on this Mac and its fleet",
    start_url: "/",
    display: "standalone",
    icons: [
      { src: "/icon.svg", sizes: "any", type: "image/svg+xml" },
      { src: "/apple-icon.png", sizes: "180x180", type: "image/png" },
    ],
  }
}
