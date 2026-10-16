// `npm run dev` / `npm start`: next on 127.0.0.1 (the API runs rekcod commands as you, so it
// never listens on other interfaces), from PORT or 4500 up to the first free port. Each run
// gets a new token unless REKCOD_DASHBOARD_TOKEN is set; open the printed /auth link.
import { spawn } from "node:child_process"
import { randomBytes } from "node:crypto"
import net from "node:net"

const mode = process.argv[2] === "start" ? "start" : "dev"
const HOST = "127.0.0.1"

const free = (port) =>
  new Promise((ok) => {
    const s = net.createServer().once("error", () => ok(false))
    s.listen(port, HOST, () => s.close(() => ok(true)))
  })

let port = Number(process.env.PORT) || 4500
while (!(await free(port))) port++
const token = process.env.REKCOD_DASHBOARD_TOKEN || randomBytes(32).toString("hex")
console.log(`rekcod dashboard: http://localhost:${port}/auth?token=${token}`)
const env = { ...process.env, REKCOD_DASHBOARD_TOKEN: token }
const child = spawn("next", [mode, "-H", HOST, "-p", String(port)], { stdio: "inherit", shell: false, env })
child.on("exit", (code) => process.exit(code ?? 0))
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => child.kill(sig))
