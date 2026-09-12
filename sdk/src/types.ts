export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json };

export interface KeyValueEntry {
  id: string;
  value: Json;
}

export interface HorizonDBOptions {
  url?: string;
  username?: string;
  password?: string;
  table?: string;
  requestTimeoutMs?: number;
}

export const DEFAULT_TABLE = "json";
export const DEFAULT_REQUEST_TIMEOUT_MS = 10_000;

export interface Packet {
  id: string;
  operation: string;
  table?: string;
  key?: string;
  value?: Json;
  amount?: number;
  element?: Json;
  sessionId?: string;
  login?: string;
  password?: string;
  defaultValue?: Json;
  time?: number;
  returnValue?: boolean;
  timeoutMs?: number;
}

/** Shape of the `data` field answered by mutating operations. */
export interface MutationResponse {
  success: boolean;
  /** `add`/`sub`: the new number; `push`/`pull` with `returnValue`: the array. */
  value?: Json;
  /** `delete`: whether the key existed before the call. */
  existed?: boolean;
}

export type ResponsePacket = {
  id: string;
  type: "response" | "error";
  data?: Json;
  error?: string;
};
