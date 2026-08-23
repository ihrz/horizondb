import {
  Connection,
  HorizonDBAuthError,
  type ConnectionOptions,
} from "./connection";
import {
  DEFAULT_REQUEST_TIMEOUT_MS,
  DEFAULT_TABLE,
  type HorizonDBOptions,
  type Json,
  type KeyValueEntry,
} from "./types";
import { requireKey, resolveTable } from "./utils";

export interface ResolvedHorizonDBOptions extends ConnectionOptions {
  table: string;
}

function resolveOptions(
  options: string | HorizonDBOptions,
): ResolvedHorizonDBOptions {
  const normalized: HorizonDBOptions =
    typeof options === "string" ? { url: options } : options;
  return {
    url: normalized.url ?? "ws://127.0.0.1:8080",
    username: normalized.username,
    password: normalized.password,
    requestTimeoutMs:
      normalized.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS,
    table: resolveTable(normalized.table),
  };
}

export class HorizonDB {
  private readonly connection: Connection;
  private readonly defaultTable: string;

  constructor(options: string | HorizonDBOptions);
  constructor(connection: Connection, table: string);
  constructor(
    options: string | HorizonDBOptions | Connection,
    table?: string,
  ) {
    if (options instanceof Connection) {
      this.connection = options;
      this.defaultTable = resolveTable(table);
      return;
    }
    const resolved = resolveOptions(options);
    this.connection = new Connection(resolved);
    this.defaultTable = resolved.table;
  }

  static async connect(
    options: string | HorizonDBOptions,
  ): Promise<HorizonDB> {
    const db = new HorizonDB(options);
    await db.ready();
    return db;
  }

  async ready(): Promise<void> {
    await this.connection.ready;
  }

  table(name: string): HorizonDB {
    return new HorizonDB(this.connection, name);
  }

  async get(key: string, defaultValue?: Json): Promise<Json | undefined>;
  async get<T = Json>(key: string, defaultValue?: Json): Promise<T | undefined>;
  async get<T = Json>(
    key: string,
    defaultValue?: Json,
  ): Promise<Json | T | undefined> {
    const data = await this.connection.request<T>("get", {
      table: this.defaultTable,
      key: requireKey(key),
      defaultValue,
    });
    if (data === null && defaultValue === undefined) {
      return undefined;
    }
    return data;
  }

  async set(key: string, value: Json): Promise<void> {
    await this.connection.request("set", {
      table: this.defaultTable,
      key: requireKey(key),
      value,
    });
  }

  async delete(key: string): Promise<void> {
    await this.connection.request("delete", {
      table: this.defaultTable,
      key: requireKey(key),
    });
  }

  async add(key: string, amount = 1): Promise<void> {
    await this.connection.request("add", {
      table: this.defaultTable,
      key: requireKey(key),
      amount,
    });
  }

  async sub(key: string, amount = 1): Promise<void> {
    await this.connection.request("sub", {
      table: this.defaultTable,
      key: requireKey(key),
      amount,
    });
  }

  async push(key: string, element: Json): Promise<void> {
    await this.connection.request("push", {
      table: this.defaultTable,
      key: requireKey(key),
      element,
    });
  }

  async pull(key: string, element: Json): Promise<void> {
    await this.connection.request("pull", {
      table: this.defaultTable,
      key: requireKey(key),
      element,
    });
  }

  async has(key: string): Promise<boolean> {
    return this.connection.request<boolean>("has", {
      table: this.defaultTable,
      key: requireKey(key),
    });
  }

  async all(): Promise<KeyValueEntry[]> {
    return this.connection.request<KeyValueEntry[]>("all", {
      table: this.defaultTable,
    });
  }

  async deleteAll(): Promise<void> {
    await this.connection.request("deleteAll", {
      table: this.defaultTable,
    });
  }

  async cache(key: string, value: Json, ttlMs = 60_000): Promise<void> {
    await this.connection.request("cache", {
      table: this.defaultTable,
      key: requireKey(key),
      value,
      time: ttlMs,
    });
  }

  async login(username: string, password: string): Promise<void> {
    await this.connection.login(username, password);
  }

  async logout(): Promise<void> {
    await this.connection.logout();
  }

  close(): void {
    this.connection.close();
  }
}

export type { HorizonDBOptions, Json, KeyValueEntry };
export { DEFAULT_REQUEST_TIMEOUT_MS, DEFAULT_TABLE };
export { HorizonDBAuthError };
