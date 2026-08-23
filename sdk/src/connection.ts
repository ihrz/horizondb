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

  private async open(): Promise<void> {
    this.socket = this.createSocket();
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
        this.socket.removeEventListener("open", onOpen);
        this.socket.removeEventListener("error", onError);
      };
      this.socket.addEventListener("open", onOpen);
      this.socket.addEventListener("error", onError);
    });
    this.socket.addEventListener("message", (event) => {
      this.handleMessage(event.data);
    });
    this.socket.addEventListener("close", () => {
      this.rejectAllPending(new ConnectionClosedError());
    });
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
      data = await this.send<{ sessionId?: string }>("login", {
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

  private send<T = Json>(
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
    this.rejectAllPending(new ConnectionClosedError());
    this.socket.close();
  }
}
