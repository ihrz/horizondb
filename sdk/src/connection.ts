import type { Json, ResponsePacket } from "./types";

interface PendingEntry {
  resolve: (value: Json) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout> | undefined;
}

export class ConnectionClosedError extends Error {
  constructor() {
    super("connection closed");
    this.name = "ConnectionClosedError";
  }
}

export interface ConnectionOptions {
  url: string;
  username?: string;
  password?: string;
  requestTimeoutMs: number;
}

type SocketLike = {
  readonly readyState: number;
  send(data: string): void;
  close(): void;
  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "error", listener: () => void): void;
  addEventListener(type: "close", listener: () => void): void;
  addEventListener(
    type: "message",
    listener: (event: { data: unknown }) => void,
  ): void;
  removeEventListener(type: "open", listener: () => void): void;
  removeEventListener(type: "error", listener: () => void): void;
  removeEventListener(type: "close", listener: () => void): void;
  removeEventListener(
    type: "message",
    listener: (event: { data: unknown }) => void,
  ): void;
};

export class HorizonDBAuthError extends Error {
  constructor(message = "invalid credentials") {
    super(message);
    this.name = "HorizonDBAuthError";
  }
}

export class HorizonDBTimeoutError extends Error {
  constructor(operation: string) {
    super(`request timed out: ${operation}`);
    this.name = "HorizonDBTimeoutError";
  }
}

export class HorizonDBOpError extends TypeError {
  constructor(message: string) {
    super(message);
    this.name = "HorizonDBOpError";
  }
}

function errorFromResponse(packet: ResponsePacket): Error {
  const message = packet.error ?? "unknown server error";
  if (/session/i.test(message)) {
    const error = new HorizonDBOpError(message);
    error.name = "HorizonDBSessionError";
    return error;
  }
  return new HorizonDBOpError(message);
}

const SOCKET_OPEN = 1;

// Reconnection backoff bounds.
const RECONNECT_BASE_DELAY_MS = 250;
const RECONNECT_MAX_DELAY_MS = 5_000;

/**
 * Races `promise` against a timeout; the timer is always cleaned up so
 * settled calls never keep the process alive.
 */
async function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  error: Error,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(error), ms);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export class Connection {
  readonly url: string;
  readonly ready: Promise<void>;
  private readonly username?: string;
  private readonly password?: string;
  private readonly requestTimeoutMs: number;
  private socket!: SocketLike;
  private readonly pending = new Map<string, PendingEntry>();
  private sessionId: string | null = null;
  private sessionValid = false;
  private explicitlyClosed = false;
  /** In-flight reconnect; requests wait for it instead of failing. */
  private reconnectGate: Promise<void> | null = null;

  constructor(options: ConnectionOptions) {
    this.url = options.url;
    this.username = options.username;
    this.password = options.password;
    this.requestTimeoutMs = options.requestTimeoutMs;
    this.ready = this.open().then(async () => {
      if (this.username !== undefined && this.password !== undefined) {
        await this.login(this.username, this.password);
      }
    });
    this.ready.catch(() => {});
  }

  static defaultSocket(url: string): SocketLike {
    return new WebSocket(url) as unknown as SocketLike;
  }

  protected createSocket(): SocketLike {
    return Connection.defaultSocket(this.url);
  }

  /**
   * Creates a fresh socket, wires the message/close handlers (before the open
   * handshake, so failed attempts also trigger the close path) and resolves
   * once the socket is open.
   */
  private async openSocket(): Promise<void> {
    const socket = this.createSocket();
    this.socket = socket;
    socket.addEventListener("message", (event) => {
      this.handleMessage(event.data);
    });
    socket.addEventListener("close", () => {
      this.handleClose(socket);
    });
    await new Promise<void>((resolve, reject) => {
      const onOpen = () => {
        cleanup();
        resolve();
      };
      const onError = () => {
        cleanup();
        reject(new Error(`unable to connect to ${this.url}`));
      };
      const cleanup = () => {
        socket.removeEventListener("open", onOpen);
        socket.removeEventListener("error", onError);
      };
      socket.addEventListener("open", onOpen);
      socket.addEventListener("error", onError);
    });
  }

  private open(): Promise<void> {
    return this.openSocket();
  }

  private handleClose(socket: SocketLike): void {
    // Ignore close events from sockets that were already superseded.
    if (this.socket !== socket) return;
    this.rejectAllPending(new ConnectionClosedError());
    this.sessionId = null;
    this.sessionValid = false;
    if (this.explicitlyClosed) return;
    this.scheduleReconnect();
  }

  private scheduleReconnect(): void {
    if (this.reconnectGate) return;
    const gate = this.reconnectWithBackoff().finally(() => {
      if (this.reconnectGate === gate) this.reconnectGate = null;
    });
    this.reconnectGate = gate;
  }

  /**
   * Re-opens the socket with exponential backoff until it succeeds or the
   * connection is explicitly closed. Re-authenticates once reconnected.
   */
  private async reconnectWithBackoff(): Promise<void> {
    let delay = RECONNECT_BASE_DELAY_MS;
    while (!this.explicitlyClosed) {
      await new Promise((resolve) => setTimeout(resolve, delay));
      delay = Math.min(delay * 2, RECONNECT_MAX_DELAY_MS);
      try {
        await this.openSocket();
        if (this.username !== undefined && this.password !== undefined) {
          await this.login(this.username, this.password);
        }
        return;
      } catch {
        // Keep retrying with backoff.
      }
    }
  }

  /**
   * Waits until the current socket is open. While a reconnect is in flight the
   * reconnect gate is awaited; otherwise the socket's own open/close events
   * drive the loop (a close means the close handler will schedule a reconnect
   * that replaces the socket).
   */
  private async waitUntilOpen(): Promise<void> {
    while (true) {
      if (this.explicitlyClosed) throw new ConnectionClosedError();
      const socket = this.socket;
      if (socket.readyState === SOCKET_OPEN) return;
      if (this.reconnectGate) {
        await this.reconnectGate.catch(() => {});
        continue;
      }
      await new Promise<void>((resolve) => {
        const onOpen = () => {
          cleanup();
          resolve();
        };
        const onClose = () => {
          cleanup();
          resolve();
        };
        const cleanup = () => {
          socket.removeEventListener("open", onOpen);
          socket.removeEventListener("close", onClose);
        };
        socket.addEventListener("open", onOpen);
        socket.addEventListener("close", onClose);
      });
    }
  }

  private handleMessage(data: unknown): void {
    let packet: ResponsePacket;
    try {
      packet = JSON.parse(String(data)) as ResponsePacket;
    } catch {
      return;
    }
    const entry = this.pending.get(packet.id);
    if (!entry) return;
    this.pending.delete(packet.id);
    if (entry.timer) clearTimeout(entry.timer);
    if (packet.type === "error") {
      entry.reject(errorFromResponse(packet));
    } else {
      entry.resolve(packet.data ?? null);
    }
  }

  private rejectAllPending(error: Error): void {
    for (const entry of this.pending.values()) {
      if (entry.timer) clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
    this.sessionValid = false;
  }

  async login(username: string, password: string): Promise<void> {
    let data: { sessionId?: string } | undefined;
    try {
      data = await this.transmit<{ sessionId?: string }>("login", {
        login: username,
        password,
      });
    } catch (error) {
      if (error instanceof Error && /credential/i.test(error.message)) {
        throw new HorizonDBAuthError();
      }
      throw error;
    }
    const sessionId = data?.sessionId;
    if (typeof sessionId !== "string") {
      throw new HorizonDBAuthError();
    }
    this.sessionId = sessionId;
    this.sessionValid = true;
  }

  async logout(): Promise<void> {
    await this.send("logout");
    this.sessionId = null;
    this.sessionValid = false;
  }

  async request<T = Json>(
    operation: string,
    fields: Record<string, unknown> = {},
  ): Promise<T> {
    try {
      return await this.send<T>(operation, fields);
    } catch (error) {
      const canReauth =
        this.username !== undefined &&
        this.password !== undefined &&
        error instanceof TypeError &&
        /session/i.test(error.message);
      if (!canReauth) throw error;
      await this.login(this.username as string, this.password as string);
      return this.send<T>(operation, fields);
    }
  }

  private async send<T = Json>(
    operation: string,
    fields: Record<string, unknown> = {},
  ): Promise<T> {
    const wait = this.waitUntilOpen();
    if (this.requestTimeoutMs > 0) {
      await withTimeout(
        wait,
        this.requestTimeoutMs,
        new HorizonDBTimeoutError(operation),
      );
    } else {
      await wait;
    }
    if (this.socket.readyState !== SOCKET_OPEN) {
      throw new ConnectionClosedError();
    }
    return this.transmit<T>(operation, fields);
  }

  /**
   * Sends a packet and correlates the answer. The caller must ensure the
   * socket is open — used by both `send` and (re-)login.
   */
  private transmit<T = Json>(
    operation: string,
    fields: Record<string, unknown> = {},
  ): Promise<T> {
    if (this.socket.readyState !== SOCKET_OPEN) {
      return Promise.reject(new ConnectionClosedError());
    }
    const id = crypto.randomUUID();
    const packet = {
      id,
      operation,
      ...fields,
      ...(this.sessionValid ? { sessionId: this.sessionId } : {}),
    };
    return new Promise<T>((resolve, reject) => {
      const useTimer = this.requestTimeoutMs > 0;
      const timer = useTimer
        ? setTimeout(() => {
            this.pending.delete(id);
            reject(new HorizonDBTimeoutError(operation));
          }, this.requestTimeoutMs)
        : undefined;
      this.pending.set(id, {
        resolve: resolve as (value: Json) => void,
        reject,
        timer: timer as ReturnType<typeof setTimeout>,
      });
      this.socket.send(JSON.stringify(packet));
    });
  }

  close(): void {
    this.explicitlyClosed = true;
    this.rejectAllPending(new ConnectionClosedError());
    this.socket.close();
  }
}
