// `rekcod dashboard` runs this, not server.js (bundle.mjs copies it next to server.js). It
// pins the address to 127.0.0.1 whatever HOSTNAME says and drops connections from any other
// peer. That doesn't stop containers, which reach the Mac's 127.0.0.1 through
// host.docker.internal; the token does (src/lib/auth.ts).
const http = require("node:http")
const path = require("node:path")

process.env.HOSTNAME = "127.0.0.1"
const loopback = (a) => /^(127\.|::1$|::ffff:127\.)/.test(a ?? "")
const listen = http.Server.prototype.listen
http.Server.prototype.listen = function (...args) {
  this.prependListener("connection", (s) => {
    if (!loopback(s.remoteAddress)) s.destroy()
  })
  return listen.apply(this, args)
}
if (!process.env.REKCOD_DASHBOARD_TOKEN) console.error("start.cjs: no REKCOD_DASHBOARD_TOKEN; every request will get 401")
require(path.join(__dirname, "server.js"))
