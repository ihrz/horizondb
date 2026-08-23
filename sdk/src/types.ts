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
}

export type ResponsePacket = {
  id: string;
  type: "response" | "error";
  data?: Json;
  error?: string;
};
