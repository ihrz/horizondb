import { DEFAULT_TABLE } from "./types";

export class InvalidTableNameError extends TypeError {
  constructor(table: string) {
    super(
      `invalid table name '${table}': expected [A-Za-z_][A-Za-z0-9_]{0,62}`,
    );
    this.name = "InvalidTableNameError";
  }
}

export function isValidTableName(table: string): boolean {
  return (
    table.length > 0 &&
    table.length <= 63 &&
    /^[A-Za-z_]/.test(table) &&
    /^[\w]+$/.test(table)
  );
}

export function resolveTable(table?: string): string {
  const candidate = table ?? DEFAULT_TABLE;
  if (!isValidTableName(candidate)) {
    throw new InvalidTableNameError(candidate);
  }
  return candidate;
}

export function requireKey(key: string | undefined): string {
  if (key === undefined || key.length === 0) {
    throw new TypeError("key is required");
  }
  return key;
}
