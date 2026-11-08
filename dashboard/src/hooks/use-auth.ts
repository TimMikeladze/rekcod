"use client"

import { useSyncExternalStore } from "react"
import { authStatus, subscribeAuth, type AuthStatus } from "@/lib/api"

/** Whether this tab holds a token the server accepts. The server render assumes it does. */
export function useAuth(): AuthStatus {
  return useSyncExternalStore(subscribeAuth, authStatus, () => "in")
}
