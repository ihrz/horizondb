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
  type MutationResponse,
} from "./types";
import { requireKey, resolveTable } from "./utils";

export interface ResolvedHorizonDBOptions extends ConnectionOptions {
  table: string;
}

/** Options for array mutations (`push`, `pull`). */
export interface ArrayMutationOptions {
  /**
   * When true the server answers with the resulting array, saving the
   * follow-up `get` a caller would otherwise issue. Off by default because
   * the array can be large.
   */
  returnValue?: boolean;
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

function numberOf(response: MutationResponse | null | undefined): number | undefined {
  return typeof response?.value === "number" ? response.value : undefined;
}

function arrayOf(response: MutationResponse | null | undefined): Json[] | undefined {
  return Array.isArray(response?.value) ? response.value : undefined;
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

  /** Number of sockets opened by the underlying connection (diagnostics). */
  get connectionCount(): number {
    return this.connection.connectionCount;
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

  /**
   * Deletes a leaf or a whole entry. Resolves to whether the key existed
   * (`undefined` when talking to a server that does not report it).
   */
  async delete(key: string): Promise<boolean | undefined> {
    const data = await this.connection.request<MutationResponse | null>(
      "delete",
      {
        table: this.defaultTable,
        key: requireKey(key),
      },
    );
    return typeof data?.existed === "boolean" ? data.existed : undefined;
  }

  /** Increments a number; resolves to the new value when the server reports it. */
  async add(key: string, amount = 1): Promise<number | undefined> {
    const data = await this.connection.request<MutationResponse | null>("add", {
      table: this.defaultTable,
      key: requireKey(key),
      amount,
    });
    return numberOf(data);
  }

  /** Decrements a number; resolves to the new value when the server reports it. */
  async sub(key: string, amount = 1): Promise<number | undefined> {
    const data = await this.connection.request<MutationResponse | null>("sub", {
      table: this.defaultTable,
      key: requireKey(key),
      amount,
    });
    return numberOf(data);
  }

  /**
   * Appends an element to an array. With `returnValue: true` resolves to
   * the resulting array (when the server reports it).
   */
  async push(
    key: string,
    element: Json,
    options: ArrayMutationOptions = {},
  ): Promise<Json[] | undefined> {
    const data = await this.connection.request<MutationResponse | null>("push", {
      table: this.defaultTable,
      key: requireKey(key),
      element,
      ...(options.returnValue ? { returnValue: true } : {}),
    });
    return arrayOf(data);
  }

  /**
   * Removes every occurrence of an element from an array. With
   * `returnValue: true` resolves to the resulting array (when the server
   * reports it).
   */
  async pull(
    key: string,
    element: Json,
    options: ArrayMutationOptions = {},
  ): Promise<Json[] | undefined> {
    const data = await this.connection.request<MutationResponse | null>("pull", {
      table: this.defaultTable,
      key: requireKey(key),
      element,
      ...(options.returnValue ? { returnValue: true } : {}),
    });
    return arrayOf(data);
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
