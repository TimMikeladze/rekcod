import type { Metadata } from "next"
import { Geist, Geist_Mono } from "next/font/google"
import { Shell } from "@/components/dashboard"
import { ThemeProvider } from "@/components/shell/theme"
import { Toaster } from "@/components/ui/sonner"
import "./globals.css"

const geistSans = Geist({ variable: "--font-geist-sans", subsets: ["latin"] })
const geistMono = Geist_Mono({ variable: "--font-geist-mono", subsets: ["latin"] })

// Every page renders from the live state the shell streams in the browser, so there's no
// server-rendered page for Next to check navigations against (each page opts out too).
export const instant = false

export const metadata: Metadata = {
  title: "rekcod",
  description: "Containers, apps, jobs and sandboxes on this Mac and its fleet",
}

// Folds the sidebar and sets the table density before first paint, so neither jumps on load.
const sidebar = `try{var d=document.documentElement.dataset;localStorage.getItem("rekcod.sidebar.collapsed")==="1"&&(d.sidebar="collapsed");localStorage.getItem("rekcod.density")==="compact"&&(d.density="compact")}catch(e){}`

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html lang="en" className={`${geistSans.variable} ${geistMono.variable} antialiased`} suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: sidebar }} />
      </head>
      <body className="min-h-screen bg-background text-foreground">
        <ThemeProvider>
          <Shell>{children}</Shell>
          <Toaster position="bottom-right" />
        </ThemeProvider>
      </body>
    </html>
  )
}
