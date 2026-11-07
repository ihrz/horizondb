<p align="center">
  <img src="assets/banner.svg" alt="HorizonDB — clé-valeur JSON sur WebSocket. La mémoire au-dessus, PostgreSQL en dessous." width="100%">
</p>

<p align="center">
  <b>Le serveur clé-valeur qui fait tourner iHorizon.</b><br>
  Lectures et écritures à la vitesse de la mémoire, versées par lots dans PostgreSQL en coulisses,<br>
  le tout sur un seul WebSocket depuis un client Bun sans dépendance.
</p>

<p align="center">
  <a href="#démarrage-rapide">Démarrage rapide</a> ·
  <a href="#comment-ça-marche">Fonctionnement</a> ·
  <a href="#le-protocole">Protocole</a> ·
  <a href="sdk/SDK_GUIDE.md">Guide du SDK</a> ·
  <a href="benchmarks/BENCHMARKS.md">Benchmarks</a> ·
  <a href="README.md">English</a>
</p>

<p align="center">
  <img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-FF7A45?style=flat-square&logo=rust&logoColor=white&labelColor=11153A">
  <img alt="unsafe interdit" src="https://img.shields.io/badge/unsafe-interdit-FF5A7A?style=flat-square&labelColor=11153A">
  <img alt="PostgreSQL write-behind" src="https://img.shields.io/badge/PostgreSQL-write--behind-FFB347?style=flat-square&logo=postgresql&logoColor=white&labelColor=11153A">
  <img alt="SDK Bun, zéro dépendance" src="https://img.shields.io/badge/SDK-Bun%20·%200%20dép.-FFE7A3?style=flat-square&logo=bun&logoColor=white&labelColor=11153A">
</p>

---

```ts
import { HorizonDB } from "horizondb";

const db = await HorizonDB.connect({ url: "ws://127.0.0.1:8080", username: "admin", password: "secret" });

await db.set("guild.42.settings.prefix", "!");                 // les clés pointées fusionnent dans un document
await db.add("guild.42.stats.messages");                       // → 1, la nouvelle valeur, sans relecture
await db.push("guild.42.mods", "1234", { returnValue: true }); // → ["1234"]
await db.cache("cooldown.42.daily", true, 86_400_000);         // disparaît dans 24 h

console.log(await db.get("guild.42"));
// { settings: { prefix: "!" }, stats: { messages: 1 }, mods: ["1234"] }
```

## Pourquoi HorizonDB

Un bot Discord martèle quelques gros documents JSON (une guilde, un membre, un
compteur) depuis de nombreux shards en même temps. Un aller-retour SQL à
chaque `add` ou `push`, c'est lent ; un stockage purement en mémoire oublie
tout au redémarrage. HorizonDB se tient sur la ligne entre les deux.

|  |  |
| --- | --- |
| ☀️ **La mémoire au-dessus** | Chaque lecture et chaque écriture est servie depuis la mémoire. Un `push` dans un document de guilde de plusieurs mégaoctets le modifie sur place en quelques microsecondes : aucun document n'est copié en entier sur le chemin chaud. |
| 🌅 **L'horizon** | Une tâche de fond verse les clés modifiées dans PostgreSQL par lots (upserts `UNNEST`, suppressions `ANY`) toutes les `flush_interval_secs`. Un arrêt propre vide tout d'abord. |
| 🌊 **PostgreSQL en dessous** | Vos données finissent dans des tables simples (`id`, `value` JSON) que vous interrogez, sauvegardez et restaurez avec vos outils habituels. Les clés se chargent à la demande, au premier accès. |
| 🔒 **Aucune mise à jour perdue** | Les opérations lecture-modification-écriture sont sérialisées par clé. Seize workers sur un même compteur tombent pile sur la bonne somme, vérifié à chaque passe de benchmark. |
| 🧭 **Clés imbriquées** | `user.profile.name` adresse l'intérieur d'un document par clé de base, jusqu'à 32 niveaux. Une lecture ne copie que le sous-arbre demandé. |
| ⚡ **Des réponses, pas des accusés** | Les mutations renvoient leur résultat : `add`/`sub` le nouveau nombre, `delete` si la clé existait, `push`/`pull` le tableau obtenu sur demande. Pas de `get` derrière. |
| 🛡️ **Durci par défaut** | `unsafe` interdit, clippy pedantic propre, authentification argon2id ou à temps constant, noms de tables validés, messages bornés, contre-pression par connexion, échéances, pings des liens inactifs. |

## Comment ça marche

```
  client ──ws──▶ ┌───────────────────────────────────────────────┐
                 │  mémoire     ☀  documents, sessions, clés TTL │  chaque get / set / add / push
  client ──ws──▶ │              ≡  verrous par clé, marques sales│  arrive ici et répond d'ici
                 ├──────── flush toutes les N s, et à l'arrêt ───┤  ← l'horizon
                 │  PostgreSQL  ▭  une table par espace de noms  │  upserts et suppressions par lots
                 └───────────────────────────────────────────────┘
```

1. Un client se connecte et reçoit un `sessionId`. Chaque requête est un
   paquet JSON corrélé par `id` : un seul socket enchaîne des centaines de
   requêtes en parallèle.
2. La requête s'exécute sur le stockage en mémoire. Une clé pas encore chargée
   est lue une seule fois dans PostgreSQL (single-flight, parsée hors du
   runtime async), puis reste chaude.
3. Une écriture marque la clé comme sale et répond immédiatement.
4. Toutes les `flush_interval_secs` (300 par défaut), les clés sales
   descendent par lots : au plus 500 lignes ou 32 Mio par upsert, 5 000 ids
   par suppression. Une écriture arrivée pendant un flush reste sale pour le
   suivant.
5. Chaque minute, un balayage garde la mémoire proportionnelle aux données
   actives :
   - les documents inactifs depuis `pack_idle_secs` (120 par défaut) sont
     compactés en JSON texte, environ 10 fois plus petit que l'arbre parsé,
     puis reparsés sur place au prochain accès (quelques microsecondes, sans
     aller-retour PostgreSQL) ;
   - les documents propres inactifs depuis `idle_eviction_secs` (900 par
     défaut) quittent la mémoire et sont rechargés depuis PostgreSQL au
     besoin. Une clé sale ne quitte jamais la mémoire avant son flush ;
   - les clés TTL expirées sont purgées.

   L'allocateur (jemalloc) rend les pages libérées au système, et la ligne de
   stats affiche `memory_entries`, `memory_packed`, `memory_evicted`,
   `allocated` et `resident`.

> [!WARNING]
> **La durabilité est un compromis que vous choisissez.** Jusqu'au prochain
> flush, les écritures ne vivent qu'en mémoire : un crash ou un `kill -9` en
> perd jusqu'à `flush_interval_secs`. SIGINT et SIGTERM vident tout avant de
> quitter. Baissez l'intervalle si cette fenêtre est trop large pour vos
> données.

## Démarrage rapide

Il faut Rust (stable) et une instance PostgreSQL. Le script d'aide lance le
conteneur de base de données et le serveur ensemble :

```bash
./scripts/dev-up.sh      # conteneur postgres + config.toml + build + démarrage (logs dans .server.log)
./scripts/dev-down.sh    # arrêt (--all supprime aussi le conteneur)
```

Ou à la main :

```bash
cp example.config.toml config.toml   # renseignez les identifiants PostgreSQL et le secret d'auth
cargo run --release                  # écoute sur ws://127.0.0.1:8080 par défaut
cargo run --release -- other.toml    # chemin de config explicite
```

En production, [`horizondb.service`](horizondb.service) est une unité systemd
durcie qui laisse au flush final le temps de se terminer.

### Installer le SDK

Le client Bun vit dans [`sdk/`](sdk/) et n'a **aucune dépendance à
l'exécution**. La racine du dépôt l'exporte déjà, une dépendance git suffit :

```json
{
  "dependencies": {
    "horizondb": "git+https://gitlab.com/ihrz/db.git#main"
  }
}
```

```bash
bun install
```

Bun exécute le TypeScript directement. Épinglez un tag plutôt que `main` pour
des builds reproductibles (`#v1.0.0`) ; pour un dépôt privé, passez par SSH
(`git+ssh://git@gitlab.com/ihrz/db.git#main`) ou un deploy token dans l'URL.

Le client se ré-authentifie une fois quand la session expire, se reconnecte
avec backoff quand un socket devient muet, partage une seule connexion entre
les vues `table()` et transforme chaque échec en erreur typée. Tout est dans le
[guide du SDK](sdk/SDK_GUIDE.md).

## Le protocole

N'importe quel langage qui parle WebSocket et JSON peut être client. Les
requêtes portent un `id` et une `operation` ; les réponses reprennent le même
`id` avec `type: "response" | "error"`.

```jsonc
→ { "id": "1", "operation": "login", "login": "admin", "password": "secret" }
← { "id": "1", "type": "response", "data": { "success": true, "sessionId": "9f1c…" } }

→ { "id": "2", "operation": "add", "sessionId": "9f1c…", "table": "json", "key": "guild.42.stats.messages", "amount": 1 }
← { "id": "2", "type": "response", "data": { "success": true, "value": 2 } }
```

| Opération | Effet | Répond avec |
| --- | --- | --- |
| `login` / `logout` | ouvre ou ferme une session | `sessionId` |
| `get` | lit une clé ou un chemin imbriqué, avec `defaultValue` optionnel | la valeur |
| `set` | écrit ; un chemin imbriqué fusionne dans le document | `success` |
| `delete` | supprime une feuille ou une entrée entière | `existed` |
| `add` / `sub` | incrément numérique (±1 par défaut, une clé absente part de 0) | la nouvelle `value` |
| `push` / `pull` | ajoute à un tableau / retire toutes les occurrences | le tableau, avec `"returnValue": true` |
| `has` | test d'existence (un `null` stocké compte comme absent) | booléen |
| `all` | tout le contenu de la table, 10 000 lignes au plus | `[{ id, value }]` |
| `deleteAll` | vide la table | `success` |
| `cache` | écrit avec un TTL en ms (30 jours au plus), supprimé à l'expiration | `success` |

Une requête peut porter `"timeoutMs"` : si elle a déjà attendu plus longtemps
avant de pouvoir s'exécuter, le serveur répond `deadline exceeded` au lieu de
travailler pour un appelant qui a abandonné. Sémantique complète dans
[WHAT.md](WHAT.md).

## Configuration

`config.toml`, commenté dans [`example.config.toml`](example.config.toml) :

| Section | Clés | Notes |
| --- | --- | --- |
| `[server]` | `host`, `port`, `max_connections`, `max_message_size` | Dimensionnez `max_message_size` au-dessus de votre plus gros document (64 Mio par défaut, 256 Mio max) : un message trop gros ferme la connexion et tout ce qui y est en vol. |
| `[database]` | connexion, taille et timeouts du pool, `flush_interval_secs`, `pack_idle_secs`, `idle_eviction_secs` | `max_connections` doit suivre le nombre de clients qui font des lectures à froid au démarrage. `0` désactive le compactage ou l'éviction. |
| `[auth]` | `username`, `password` **ou** `password_hash`, `session_timeout_secs` | Préférez un `password_hash` argon2id en production. |

Les logs suivent `RUST_LOG` : `debug` trace chaque requête (opération, table,
clé, durée) et chaque lot de flush, `trace` chaque appel au dépôt.

## Performances

Mesurées de bout en bout via le SDK contre un vrai serveur et PostgreSQL,
réseau compris, sur un portable avec un build *debug*
([méthode et tableau complet](benchmarks/BENCHMARKS.md)) :

| Chemin | Débit | p50 |
| --- | --- | --- |
| `get` chaud, une connexion | 18 k ops/s | 42 µs |
| `has`, une connexion | 20 k ops/s | 39 µs |
| login + logout | 17 k ops/s | 43 µs |
| client WebSocket brut, même serveur | 48 k ops/s | — |

Lancez les vôtres avec `./scripts/bench.sh --quick`. La suite est bornée dans
le temps et échoue à la moindre erreur ou invariant cassé : elle tient en CI.

## Développement

```bash
./scripts/test.sh            # tests Rust + typecheck et tests du SDK (hermétiques, sans serveur)
cargo clippy --all-targets   # pedantic, attendu propre
cargo fmt --check
./scripts/bench.sh --quick   # suite de performance bornée
```

Les notes de conception sont dans [WHAT.md](WHAT.md) ; la page d'accueil dans
[`site/`](site/) ; les éléments de marque et leurs règles dans
[`assets/`](assets/README.md).

<p align="center">
  <img src="assets/logo.svg" alt="" width="56"><br>
  <sub>HorizonDB fait partie du projet <b>iHorizon</b>. Privé, en cours de développement : le protocole peut encore changer.</sub>
</p>
