---
title: SDK TypeScript
description: Le client Bun de HorizonDB, ses options, ses méthodes, ses erreurs et sa reconnexion.
---

# SDK TypeScript

Le client officiel se trouve dans [`sdk/`](https://gitlab.com/ihrz/db/-/tree/main/sdk)
du dépôt. C'est une bibliothèque TypeScript ESM **pour Bun uniquement**, **sans
aucune dépendance à l'exécution** : elle utilise le `WebSocket` global de Bun et
`crypto.randomUUID`. Bun exécute directement les sources TypeScript, il n'y a
pas d'étape de build.

## Installation { #install }

La racine du dépôt exporte le SDK, une dépendance git suffit :

```json title="package.json"
{
  "dependencies": {
    "horizondb": "git+https://gitlab.com/ihrz/db.git#main"
  }
}
```

```bash
bun install
```

Épinglez un tag plutôt que `main` pour des builds reproductibles (`#v1.0.0`).
Pour un dépôt privé, passez par SSH (`git+ssh://git@gitlab.com/ihrz/db.git#main`)
ou un deploy token dans l'URL.

## Connexion { #connect }

```ts
import { HorizonDB } from "horizondb";

// Recommandé : se résout une fois le socket ouvert et le login réussi.
const db = await HorizonDB.connect({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
});
```

Le constructeur fonctionne sans `await` : la connexion démarre aussitôt et
chaque opération attend la fin du handshake (et du login).

```ts
const db = new HorizonDB("ws://127.0.0.1:8080"); // URL seule, sans identifiants
const db2 = new HorizonDB({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
  table: "json",
  requestTimeoutMs: 10_000,
});
await db2.ready();
```

Sans identifiants, le socket s'ouvre quand même, et les opérations échouent
avec `invalid or expired session` jusqu'à ce qu'un `login()` réussisse.

### Options { #options }

| Option | Type | Défaut | Rôle |
| --- | --- | --- | --- |
| `url` | `string` | `ws://127.0.0.1:8080` | Adresse du serveur. |
| `username` | `string` | aucun | `[auth] username`. Avec `password`, le client se connecte à l'ouverture et après chaque reconnexion. |
| `password` | `string` | aucun | Secret de `[auth]` (en clair, même quand le serveur stocke un hash). |
| `table` | `string` | `json` | Table par défaut de cette instance. Validée localement. |
| `requestTimeoutMs` | `number` | `10000` | Timeout par requête. Aussi envoyé au serveur dans `timeoutMs`. `0` désactive les timers et l'indication d'échéance au serveur. |

!!! warning "Quand la première connexion échoue"

    Si le serveur est injoignable, `connect()` / `ready()` rejettent avec
    `unable to connect to <url>`, mais la connexion sous-jacente continue de
    réessayer en arrière-plan. Avec `HorizonDB.connect()`, vous n'avez aucun
    moyen de l'arrêter ; pour abandonner proprement, utilisez le constructeur
    et appelez `close()` quand `ready()` rejette :

    ```ts
    const db = new HorizonDB({ url, username, password });
    try {
      await db.ready();
    } catch (error) {
      db.close();
      throw error;
    }
    ```

## Méthodes { #methods }

Toutes les méthodes de données sont asynchrones, acceptent les clés pointées
(`"a.b.c"`, résolues par le serveur, voir [clés](protocol.md#keys)) et
rejettent une clé vide localement, avec une `TypeError`, avant tout envoi.

| Méthode | Se résout en | Notes |
| --- | --- | --- |
| `HorizonDB.connect(options)` | `HorizonDB` | Ouvre et se connecte si des identifiants sont fournis. |
| `ready()` | `void` | Attend la connexion et le login initial. |
| `table(name)` | `HorizonDB` (synchrone) | Une vue sur une autre table, qui partage la même connexion. |
| `get<T>(key, defaultValue?)` | `T \| undefined` | `undefined` si absente et sans défaut. |
| `set(key, value)` | `void` | Crée ou remplace ; une clé imbriquée fusionne. |
| `delete(key)` | `boolean \| undefined` | Si la clé existait. |
| `add(key, amount = 1)` | `number \| undefined` | La nouvelle valeur. Une clé absente part de 0. |
| `sub(key, amount = 1)` | `number \| undefined` | La nouvelle valeur. |
| `push(key, element, { returnValue? })` | `Json[] \| undefined` | Le tableau obtenu avec `returnValue: true`. |
| `pull(key, element, { returnValue? })` | `Json[] \| undefined` | Retire toutes les occurrences. |
| `has(key)` | `boolean` | Un `null` stocké compte comme absent. |
| `all()` | `{ id, value }[]` | Toute la table, 10 000 lignes au plus. |
| `deleteAll()` | `void` | Vide la table courante. |
| `cache(key, value, ttlMs = 60000)` | `void` | Écrit, puis supprime après le TTL. |
| `login(username, password)` | `void` | Gestion manuelle de la session. |
| `logout()` | `void` | Révoque la session courante. |
| `close()` | `void` (synchrone) | Ferme le socket et rejette les requêtes en vol. |
| `connectionCount` | `number` (getter) | Sockets ouverts jusqu'ici ; `1` signifie aucune reconnexion. |

### Lire { #reading }

```ts
await db.set("user", { name: "Alice", tags: ["a"], profile: { age: 30 } });

await db.get("user.name");               // "Alice"
await db.get<number>("user.profile.age"); // 30
await db.get("user.missing");            // undefined
await db.get("user.missing", "n/a");     // "n/a"
await db.has("user.tags");               // true
```

!!! note "`null` se lit comme `undefined`"

    `get` renvoie `undefined` dès que le serveur répond `null` sans valeur par
    défaut : un `null` stocké et une clé absente se ressemblent. `has()`
    renvoie aussi `false` dans les deux cas. Stockez une valeur sentinelle si
    vous devez les distinguer.

### Écrire { #writing }

```ts
await db.set("guild.42.settings.prefix", "!");   // fusionne dans "guild"
await db.add("guild.42.stats.messages");          // 1
await db.add("guild.42.stats.messages", 10);      // 11
await db.sub("guild.42.stats.messages", 2);       // 9
await db.delete("guild.42.settings.prefix");      // true
await db.delete("guild.42.settings.prefix");      // false
```

### Tableaux { #arrays }

```ts
await db.push("guild.42.mods", "1234");                        // undefined
await db.push("guild.42.mods", "5678", { returnValue: true }); // ["1234", "5678"]
await db.pull("guild.42.mods", "1234", { returnValue: true }); // ["5678"]
```

`returnValue` est désactivé par défaut car le tableau peut être gros.

### Clés à durée de vie { #ttl }

```ts
await db.cache("cooldown.42.daily", true, 86_400_000); // 24 h
```

Le TTL s'applique au **document de base** (`cooldown` ici) : voir
[l'opération `cache`](protocol.md#op-cache) pour les détails.

### Tables { #tables }

`table()` renvoie une nouvelle instance liée à une autre table. Elle partage la
connexion WebSocket et la session, rien n'est rouvert :

```ts
const users = db.table("users");
await users.set("42.name", "Bob");
await db.get("42.name");     // undefined : autre table
await users.get("42.name");  // "Bob"
```

Les noms de table sont vérifiés localement contre `[A-Za-z_][A-Za-z0-9_]{0,62}`
(`InvalidTableNameError`) avant tout envoi, en plus de la vérification du
serveur.

### Parcours et nettoyage { #scans }

```ts
const rows = await db.table("users").all(); // [{ id: "42", value: { name: "Bob" } }]
await db.table("users").deleteAll();
```

## Erreurs { #errors }

| Classe | Hérite de | Levée quand |
| --- | --- | --- |
| `HorizonDBOpError` | `TypeError` | Le serveur a répondu `type: "error"`. Le message est celui du serveur (voir la [liste des erreurs](protocol.md#errors)). Les erreurs de session utilisent cette classe avec `name = "HorizonDBSessionError"`. |
| `HorizonDBAuthError` | `Error` | Le `login` a été refusé (`invalid credentials`). |
| `HorizonDBTimeoutError` | `Error` | Pas de réponse dans `requestTimeoutMs`. A une propriété `operation`. Le message se termine par `(connection not ready)` quand l'attente portait sur une reconnexion. |
| `ConnectionClosedError` | `Error` | Le socket s'est fermé, ou le SDK l'a réinitialisé, avec la requête en vol ; ou l'instance a été fermée. |
| `InvalidTableNameError` | `TypeError` | Un nom de table a été refusé localement. |

```ts
import { HorizonDBOpError, HorizonDBTimeoutError } from "horizondb";

try {
  await db.add("user.name", 5); // "Alice" n'est pas un nombre
} catch (error) {
  if (error instanceof HorizonDBOpError) {
    console.error(error.message); // "cannot add to non-numeric value"
  } else if (error instanceof HorizonDBTimeoutError) {
    console.error("timeout :", error.operation);
  } else {
    throw error;
  }
}
```

## Résilience { #resilience }

La connexion gère seule les pannes habituelles :

- **Session expirée.** Quand une opération échoue sur une erreur de session et
  que les identifiants sont connus, le client se reconnecte **une fois** et
  rejoue la requête. Les appelants simultanés partagent un seul aller-retour de
  login.
- **Socket fermé.** Les requêtes en vol sont rejetées avec
  `ConnectionClosedError`, et le client se reconnecte avec un backoff
  exponentiel (de 250 ms, doublé jusqu'à 5 s). Une fois reconnecté, il refait
  le login **avant** de libérer les requêtes en attente : elles ne doublent
  jamais le login. Les requêtes émises entre-temps attendent la reconnexion,
  dans la limite de leur propre `requestTimeoutMs`.
- **Serveur muet.** Si une requête expire alors que **rien du tout** n'a été
  reçu sur le socket pendant toute sa fenêtre, le serveur est considéré comme
  injoignable sur ce lien (processus bloqué, TCP à moitié ouvert) : le client
  abandonne le socket et se reconnecte au lieu de laisser toutes les requêtes
  suivantes expirer. Un serveur simplement lent, qui envoie encore des
  réponses, ne déclenche jamais ce mécanisme.
- **Indication d'échéance.** Chaque paquet porte `timeoutMs`, pour que le
  serveur puisse sauter une requête qu'il n'a pas pu démarrer à temps
  (`deadline exceeded`).

`logout()` et `close()` ne sont pas rejoués.

## Exports { #exports }

```ts
import {
  HorizonDB,                       // le client
  Connection,                      // le transport (usage avancé)
  HorizonDBOpError, HorizonDBAuthError, HorizonDBTimeoutError,
  ConnectionClosedError, InvalidTableNameError,
  DEFAULT_TABLE,                   // "json"
  DEFAULT_REQUEST_TIMEOUT_MS,      // 10000
  isValidTableName, resolveTable, requireKey,
  type HorizonDBOptions, type Json, type KeyValueEntry,
  type MutationResponse, type ResponsePacket,
  type ArrayMutationOptions, type ResolvedHorizonDBOptions, type ConnectionOptions,
} from "horizondb";
```

`Json` est le type JSON récursif (`null | boolean | number | string | Json[] |
{ [key: string]: Json }`), `KeyValueEntry` vaut `{ id: string; value: Json }`.

## Développer le SDK { #development }

```bash
cd sdk
bun install        # dépendances de développement uniquement (@types/bun, typescript)
bun test           # hermétique : un mock du protocole en mémoire, sans serveur
bun run typecheck  # tsc --noEmit, strict
```

`sdk/tests/helpers/mock-server.ts` reproduit la sémantique du serveur Rust,
bizarreries comprises. Quand le comportement du serveur change, mettez d'abord
le mock à jour.
