---
title: Démarrage
description: Lancer HorizonDB en local, le compiler depuis les sources et envoyer une première requête.
---

# Démarrage

Cette page vous mène d'un clone tout frais à une première requête : un
PostgreSQL local, un serveur qui tourne, et le SDK TypeScript qui lui parle.

## Prérequis { #requirements }

| Quoi | Pourquoi |
| --- | --- |
| **Rust**, toolchain stable récente | Le serveur utilise l'édition 2024. |
| Une **toolchain C** (`cc`, `make`) | L'allocateur jemalloc est compilé depuis les sources par `tikv-jemalloc-sys`. |
| **PostgreSQL** | Le stockage durable. Le script de développement lance PostgreSQL 16 dans un conteneur. |
| **Docker** (ou une CLI compatible `docker`) | Uniquement pour `scripts/dev-up.sh`. |
| **Bun** ≥ 1.1 | Uniquement pour le SDK TypeScript (champ `engines` de `sdk/package.json`). La suite de benchmarks demande Bun ≥ 1.4. |

## Lancer avec le script de développement { #dev-script }

Le chemin le plus court vers un serveur fonctionnel est le script d'aide à la
racine du dépôt. Il lance PostgreSQL et le serveur ensemble :

```bash
git clone https://gitlab.com/ihrz/db.git horizondb
cd horizondb
./scripts/dev-up.sh      # conteneur postgres + config.toml + build + démarrage
./scripts/dev-down.sh    # arrête le serveur (--all supprime aussi le conteneur)
```

Ce que fait `dev-up.sh`, étape par étape :

1. Démarre un conteneur nommé `horizondb-pg` (`postgres:16-alpine`) sur le
   réseau de l'hôte, à l'écoute sur `127.0.0.1:55432`, avec base, utilisateur
   et mot de passe tous à `horizon`, et `synchronous_commit=off`.
2. Attend `pg_isready`.
3. S'il n'existe pas encore de `config.toml`, en génère un à partir de
   `example.config.toml` en remplaçant le port de la base par `55432`.
4. Compile un binaire de **debug** (`cargo build`) et le lance en arrière-plan,
   avec ses logs dans `.server.log` et son pid dans `.server.pid`.
5. Attend la ligne de log `HorizonDB listening` et affiche
   `HorizonDB ready on ws://127.0.0.1:8080`.

Le script lit trois variables d'environnement :

| Variable | Défaut | Rôle |
| --- | --- | --- |
| `HORIZON_PG_CONTAINER` | `horizondb-pg` | Nom du conteneur (aussi utilisé par `dev-down.sh --all`). |
| `HORIZON_PG_PORT` | `55432` | Port d'écoute de PostgreSQL. |
| `HORIZON_PORT` | `8080` | Port affiché dans le message « ready ». Il ne change pas le port du serveur, qui vient de `config.toml`. |

!!! note "`synchronous_commit=off`, c'est pour le développement"

    Le conteneur désactive le fsync au commit pour que les benchmarks mesurent
    l'application plutôt que le disque. Ne reportez pas ce réglage sur un
    PostgreSQL de production sans savoir ce qu'il sacrifie.

## Compiler depuis les sources { #build }

```bash
cp example.config.toml config.toml   # renseignez les identifiants PostgreSQL et le secret d'auth
cargo build --release                # target/release/horizondb
./target/release/horizondb           # lit ./config.toml
./target/release/horizondb /etc/horizondb/config.toml   # chemin explicite
```

`cargo run --release` et `cargo run --release -- other.toml` marchent aussi. Le
seul argument de la ligne de commande est le chemin du fichier de
configuration ; sans lui, le serveur lit `config.toml` dans le répertoire
courant.

Le profil release active la LTO, une seule unité de codegen et
`panic = "abort"`.

Au démarrage, le serveur :

1. charge et valide la configuration (toute erreur termine le processus avec le
   code 1 et une ligne `loading configuration file … failed`) ;
2. ouvre le pool PostgreSQL et exécute `SELECT 1` ;
3. ouvre l'écoute WebSocket (en réessayant jusqu'à 15 s si le port est encore
   tenu par l'instance précédente) et logue `HorizonDB listening`.

Les tables ne sont pas créées à l'avance : chacune l'est avec
`CREATE TABLE IF NOT EXISTS` la première fois qu'une requête la nomme.

## Votre première requête { #first-request }

### Avec le SDK

Le client Bun n'a aucune dépendance à l'exécution et la racine du dépôt
l'exporte, une dépendance git suffit :

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

```ts title="hello.ts"
import { HorizonDB } from "horizondb";

const db = await HorizonDB.connect({
  url: "ws://127.0.0.1:8080",
  username: "admin",     // [auth] username
  password: "secret",    // [auth] password
});

await db.set("user.name", "Alice");
console.log(await db.get("user.name")); // "Alice"
console.log(await db.add("user.visits")); // 1

db.close();
```

```bash
bun run hello.ts
```

### À la main

N'importe quel client WebSocket qui envoie des trames texte fait l'affaire.
Avec [`websocat`](https://github.com/vi/websocat), par exemple, collez un
paquet JSON par ligne :

```text
$ websocat ws://127.0.0.1:8080
{"id":"1","operation":"login","login":"admin","password":"secret"}
{"id":"1","type":"response","data":{"success":true,"sessionId":"…"},"error":null}
{"id":"2","operation":"set","sessionId":"…","key":"user.name","value":"Alice"}
{"id":"2","type":"response","data":{"success":true},"error":null}
```

La [référence du protocole](protocol.md) détaille chaque opération.

## Le fichier de configuration { #config-file }

`config.toml` a trois sections. Un fichier minimal ne contient que les champs
sans valeur par défaut :

```toml title="config.toml"
[server]
host = "127.0.0.1"
port = 8080

[database]
host = "127.0.0.1"
port = 5432
database = "horizon"
username = "horizon"
password = "change-me"

[auth]
username = "admin"
password = "secret"
```

Tout le reste (taille du pool, intervalle de flush, politique mémoire, durée
des sessions, taille des messages) a une valeur par défaut. La page
[Configuration](configuration.md) détaille chaque option.

## Et ensuite

- Lisez [Architecture](architecture.md) pour comprendre ce que « write-behind »
  implique pour la durabilité avant d'y mettre de vraies données.
- Déployez avec [l'unité systemd](operations.md#systemd).
- Lancez la [suite de benchmarks](benchmarks.md#reproduce) sur votre machine.
