import type { Json, Packet, ResponsePacket } from "../../src/types";

export interface MockServerOptions {
  username?: string;
  password?: string;
  /** Fixed port (default: random). Lets a test restart the server in place. */
  port?: number;
}

const MAX_KEY_DEPTH = 32;
const MAX_KEY_BYTES = 255;
const MAX_TTL_MS = 30 * 24 * 60 * 60 * 1000;

type Table = Map<string, string>;

function parseKey(key: string): [string, string[]] {
  const segments = key.split(".");
  if (key.length === 0 || segments.some((s) => s.length === 0)) {
    throw new Error("malformed key");
  }
  if (segments.length > MAX_KEY_DEPTH) throw new Error("key nesting too deep");
  if (key.length > MAX_KEY_BYTES) throw new Error("key too long");
  return [segments[0] as string, segments.slice(1)];
}

function walk(doc: Json, path: string[]): { found: boolean; value: Json } {
  let current = doc;
  for (const segment of path) {
    if (
      current === null ||
      typeof current !== "object" ||
      Array.isArray(current) ||
      !(segment in current)
    ) {
      return { found: false, value: null };
    }
    current = (current as Record<string, Json>)[segment] as Json;
  }
  return { found: true, value: current };
}

function isPlainObject(value: Json): value is Record<string, Json> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function sameValue(left: Json, right: Json): boolean {
  return left === right || JSON.stringify(left) === JSON.stringify(right);
}

function setNested(node: Json, path: string[], value: Json): Json {
  if (path.length === 0) return value;
  const head = path[0];
  if (head === undefined) return value;
  const rest = path.slice(1);
  if (rest.length === 0) {
    if (!isPlainObject(node)) {
      throw new TypeError("cannot set nested value on non-object");
    }
    node[head] = value;
    return node;
  }
  if (!isPlainObject(node)) {
    return setNested({}, path, value);
  }
  if (!(head in node)) {
    node[head] = {};
  }
  node[head] = setNested(node[head] as Json, rest, value);
  return node;
}

function removePath(doc: Json, path: string[]): boolean {
  const parentProbe = walk(doc, path.slice(0, -1));
  if (!parentProbe.found || !isPlainObject(parentProbe.value)) {
    return false;
  }
  return delete parentProbe.value[path[path.length - 1] as string];
}

export class MockHorizonServer {
  readonly url: string;
  private readonly tables = new Map<string, Table>();
  private readonly sessions = new Set<string>();
  private readonly timers: ReturnType<typeof setTimeout>[] = [];
  private server: Bun.Server<undefined>;
  /** Sockets accepted so far. */
  openCount = 0;
  /** Successful logins so far. */
  loginCount = 0;
  /** While true every incoming packet is dropped without an answer. */
  silent = false;

  constructor(options: MockServerOptions = {}) {
    const username = options.username ?? "admin";
    const password = options.password ?? "secret";
    const self = this;

    this.server = Bun.serve<undefined>({
      port: options.port ?? 0,
      fetch(request, server) {
        if (server.upgrade(request)) {
          return;
        }
        return new Response("websocket upgrade required", { status: 426 });
      },
      websocket: {
        open() {
          self.openCount += 1;
        },
        message(ws, message) {
          if (self.silent) return;
          const raw =
            typeof message === "string"
              ? message
              : Buffer.from(message).toString();
          let packet: Packet;
          try {
            packet = JSON.parse(raw) as Packet;
          } catch {
            ws.send(
              JSON.stringify({
                id: "unknown",
                type: "error",
                error: "Invalid JSON",
              } satisfies ResponsePacket),
            );
            return;
          }
          ws.send(JSON.stringify(self.handle(packet, username, password)));
        },
      },
    });
    this.url = `ws://127.0.0.1:${this.server.port}`;
  }

  get port(): number {
    return this.server.port as number;
  }

  stop(): void {
    for (const timer of this.timers) clearTimeout(timer);
    this.server.stop(true);
  }

  rowcount(table: string): number {
    return this.tables.get(table)?.size ?? 0;
  }

  private table(name: string): Table {
    let rows = this.tables.get(name);
    if (!rows) {
      rows = new Map();
      this.tables.set(name, rows);
    }
    return rows;
  }

  private handle(
    packet: Packet,
    username: string,
    password: string,
  ): ResponsePacket {
    try {
      switch (packet.operation) {
        case "login":
          return this.login(packet, username, password);
        case "logout":
          if (packet.sessionId) this.sessions.delete(packet.sessionId);
          return ok(packet.id, { success: true });
        default:
          break;
      }
      if (!packet.sessionId || !this.sessions.has(packet.sessionId)) {
        return fail(packet.id, "invalid or expired session");
      }
      return this.dispatchAuthenticated(packet);
    } catch (error) {
      return fail(packet.id, error instanceof Error ? error.message : String(error));
    }
  }

  private login(
    packet: Packet,
    username: string,
    password: string,
  ): ResponsePacket {
    if (packet.login === username && packet.password === password) {
      const sessionId = crypto.randomUUID();
      this.sessions.add(sessionId);
      this.loginCount += 1;
      return ok(packet.id, { success: true, sessionId });
    }
    return fail(packet.id, "invalid credentials");
  }

  private dispatchAuthenticated(packet: Packet): ResponsePacket {
    const table = packet.table ?? "json";
    switch (packet.operation) {
      case "get":
        return this.get(packet, table);
      case "set":
        return this.set(packet, table);
      case "delete":
        return this.delete(packet, table);
      case "add":
        return this.adjust(packet, table, packet.amount ?? 1);
      case "sub":
        return this.adjust(packet, table, -(packet.amount ?? 1));
      case "push":
        return this.push(packet, table);
      case "pull":
        return this.pull(packet, table);
      case "has":
        return this.has(packet, table);
      case "all":
        return this.all(packet, table);
      case "deleteAll":
        this.tables.set(table, new Map());
        return ok(packet.id, { success: true });
      case "cache":
        return this.cache(packet, table);
      default:
        return fail(packet.id, `Unknown operation: ${packet.operation}`);
    }
  }

  private readDoc(table: string, base: string): Json | undefined {
    const raw = this.table(table).get(base);
    return raw === undefined ? undefined : (JSON.parse(raw) as Json);
  }

  private writeDoc(table: string, base: string, doc: Json): void {
    this.table(table).set(base, JSON.stringify(doc));
  }

  private get(packet: Packet, table: string): ResponsePacket {
    const [base, path] = parseKey(packet.key as string);
    const stored = this.readDoc(table, base);
    if (stored === undefined) {
      return ok(packet.id, packet.defaultValue ?? null);
    }
    const result = walk(stored, path);
    return ok(packet.id, result.found ? result.value : (packet.defaultValue ?? null));
  }

  private set(packet: Packet, table: string): ResponsePacket {
    const [base, path] = parseKey(packet.key as string);
    const value = packet.value ?? null;
    if (path.length === 0) {
      this.writeDoc(table, base, value);
      return ok(packet.id, { success: true });
    }
    const doc: Json = this.readDoc(table, base) ?? {};
    let merged: Json;
    try {
      merged = setNested(doc, path, value);
    } catch (error) {
      return fail(packet.id, (error as Error).message);
    }
    this.writeDoc(table, base, merged);
    return ok(packet.id, { success: true });
  }

  private delete(packet: Packet, table: string): ResponsePacket {
    const [base, path] = parseKey(packet.key as string);
    const rows = this.table(table);
    if (path.length === 0) {
      const existed = rows.delete(base);
      return ok(packet.id, { success: true, existed });
    }
    const stored = this.readDoc(table, base);
    if (stored === undefined) {
      return ok(packet.id, { success: true, existed: false });
    }
    const existed = removePath(stored, path);
    if (isPlainObject(stored) && Object.keys(stored).length === 0) {
      rows.delete(base);
    } else {
      this.writeDoc(table, base, stored);
    }
    return ok(packet.id, { success: true, existed });
  }

  private adjust(packet: Packet, table: string, amount: number): ResponsePacket {
    if (typeof amount !== "number" || !Number.isFinite(amount)) {
      return fail(packet.id, "amount must be finite");
    }
    const [base, path] = parseKey(packet.key as string);
    const stored = this.readDoc(table, base);
    let updated: Json;
    if (stored === undefined && path.length === 0) {
      updated = amount;
    } else {
      const doc: Json = stored ?? {};
      const probe = walk(doc, path);
      if (probe.found) {
        if (typeof probe.value !== "number") {
          return fail(packet.id, "cannot add to non-numeric value");
        }
        try {
          updated = setNested(doc, path, probe.value + amount);
        } catch (error) {
          return fail(packet.id, (error as Error).message);
        }
      } else {
        try {
          updated = setNested(doc, path, amount);
        } catch (error) {
          return fail(packet.id, (error as Error).message);
        }
      }
    }
    this.writeDoc(table, base, updated);
    const result = walk(updated, path);
    return ok(packet.id, { success: true, value: result.found ? result.value : null });
  }

  private push(packet: Packet, table: string): ResponsePacket {
    const [base, path] = parseKey(packet.key as string);
    const stored = this.readDoc(table, base);
    const doc: Json = stored ?? (path.length === 0 ? [] : {});
    const probe = walk(doc, path);
    if (probe.found && !Array.isArray(probe.value)) {
      return fail(packet.id, "target value is not an array");
    }
    const items = probe.found ? [...(probe.value as Json[])] : [];
    items.push(packet.element ?? null);
    try {
      this.writeDoc(table, base, setNested(doc, path, items));
    } catch (error) {
      return fail(packet.id, (error as Error).message);
    }
    return ok(
      packet.id,
      packet.returnValue ? { success: true, value: items } : { success: true },
    );
  }

  private pull(packet: Packet, table: string): ResponsePacket {
    const [base, path] = parseKey(packet.key as string);
    const stored = this.readDoc(table, base);
    const doc: Json = stored ?? (path.length === 0 ? [] : {});
    const probe = walk(doc, path);
    if (probe.found && !Array.isArray(probe.value)) {
      return fail(packet.id, "target value is not an array");
    }
    const items = probe.found ? (probe.value as Json[]) : [];
    const element = packet.element ?? null;
    const remaining = items.filter((item) => !sameValue(item, element));
    try {
      this.writeDoc(table, base, setNested(doc, path, remaining));
    } catch (error) {
      return fail(packet.id, (error as Error).message);
    }
    return ok(
      packet.id,
      packet.returnValue ? { success: true, value: remaining } : { success: true },
    );
  }

  private has(packet: Packet, table: string): ResponsePacket {
    const [base, path] = parseKey(packet.key as string);
    const stored = this.readDoc(table, base);
    if (stored === undefined) return ok(packet.id, false);
    const result = walk(stored, path);
    return ok(packet.id, result.found && result.value !== null);
  }

  private all(packet: Packet, table: string): ResponsePacket {
    const rows = this.tables.get(table) ?? new Map();
    const entries = [...rows.entries()]
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([id, raw]) => ({
        id,
        value: JSON.parse(raw) as Json,
      }));
    return ok(packet.id, entries);
  }

  private cache(packet: Packet, table: string): ResponsePacket {
    const ttlMs = packet.time ?? 60_000;
    if (
      typeof ttlMs !== "number" ||
      !Number.isFinite(ttlMs) ||
      ttlMs <= 0 ||
      ttlMs > MAX_TTL_MS
    ) {
      return fail(packet.id, "ttl must be between 1ms and 30 days");
    }
    this.set(packet, table);
    const [base] = parseKey(packet.key as string);
    const timer = setTimeout(() => {
      this.tables.get(table)?.delete(base);
    }, ttlMs);
    if (typeof timer.unref === "function") timer.unref();
    this.timers.push(timer);
    return ok(packet.id, { success: true });
  }
}

function ok(id: string, data: Json): ResponsePacket {
  return { id, type: "response", data };
}

function fail(id: string, error: string): ResponsePacket {
  return { id, type: "error", error };
}
