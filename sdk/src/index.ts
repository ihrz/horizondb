export {
  Connection,
  ConnectionClosedError,
  HorizonDBAuthError,
  HorizonDBOpError,
  HorizonDBTimeoutError,
  type ConnectionOptions,
} from "./connection";
export {
  HorizonDB,
  type ArrayMutationOptions,
  type ResolvedHorizonDBOptions,
} from "./client";
export {
  DEFAULT_REQUEST_TIMEOUT_MS,
  DEFAULT_TABLE,
  type HorizonDBOptions,
  type Json,
  type KeyValueEntry,
  type MutationResponse,
  type ResponsePacket,
} from "./types";
export {
  InvalidTableNameError,
  isValidTableName,
  requireKey,
  resolveTable,
} from "./utils";
