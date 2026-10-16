// `npm run dev` / `npm start`: next on 127.0.0.1 (the API runs rekcod commands as you, so it
// never listens on other interfaces), from PORT or 4500 up to the first free port.
import { spawn } from "node:child_process"
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
console.log(`rekcod dashboard: http://localhost:${port}`)
const child = spawn("next", [mode, "-H", HOST, "-p", String(port)], { stdio: "inherit", shell: false, env: process.env })
child.on("exit", (code) => process.exit(code ?? 0))
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => child.kill(sig))
