---
title: Configuration
description: Chaque option de config.toml, son type, sa valeur par défaut et sa règle de validation.
---

# Configuration

HorizonDB lit un seul fichier TOML. Son chemin est le premier argument de la
ligne de commande ; par défaut, `config.toml` dans le répertoire courant.

```bash
horizondb                        # ./config.toml
horizondb /etc/horizondb/config.toml
```

Le fichier est validé avant toute autre chose. Un champ manquant, un mauvais
type ou une règle non respectée arrête le processus avec le code 1 et une ligne
de log comme :

```text
ERROR loading configuration file config.toml failed: configuration error: database.flush_interval_secs must be greater than zero
```

!!! warning "Les clés inconnues sont ignorées"

    Le parseur ne rejette pas les clés qu'il ne connaît pas. Une faute de
    frappe comme `idle_evicton_secs = 1800` disparaît en silence et l'option
    garde sa valeur par défaut. Vérifiez l'orthographe avec les tableaux
    ci-dessous.

## `[server]` { #server }

L'écoute WebSocket.

| Option | Type | Défaut | Validation | Rôle |
| --- | --- | --- | --- | --- |
| `host` | chaîne | *obligatoire* | non vide | Adresse d'écoute, par exemple `127.0.0.1` ou `0.0.0.0`. |
| `port` | entier (0–65535) | *obligatoire* | | Port TCP de l'écoute WebSocket. |
| `max_connections` | entier | `1024` | > 0 | Connexions clientes simultanées. Une connexion au-delà est acceptée au niveau TCP puis fermée aussitôt (log `connection limit reached, rejecting`). |
| `max_message_size` | entier, octets | `67108864` (64 Mio) | 1 Kio – 256 Mio | Plus gros message **et** plus grosse trame WebSocket acceptés d'un client. Un message plus gros ferme la connexion de ce client, avec toutes les requêtes qu'il a en vol. Dimensionnez-la au-dessus de votre plus gros document. |

## `[database]` { #database }

La connexion PostgreSQL, le pool, et les politiques de write-behind et de
mémoire.

### Connexion et pool

| Option | Type | Défaut | Validation | Rôle |
| --- | --- | --- | --- | --- |
| `host` | chaîne | *obligatoire* | | Hôte PostgreSQL. |
| `port` | entier | *obligatoire* | | Port PostgreSQL. |
| `database` | chaîne | *obligatoire* | | Nom de la base. |
| `username` | chaîne | *obligatoire* | | Rôle utilisé par le serveur (encodé pour l'URL automatiquement). |
| `password` | chaîne | *obligatoire* | | Mot de passe du rôle (encodé pour l'URL automatiquement, masqué dans les sorties de debug). |
| `max_connections` | entier | `32` | ≥ `min_connections` | Taille maximale du pool. Le pool sert aux lectures des clés pas encore en mémoire (chargements à froid, vérifications de `has` / `delete` sur des clés inconnues, parcours `all`), à la création des tables et au flush. Dimensionnez-le pour le nombre de clients qui lisent à froid en même temps, en général juste après un redémarrage. |
| `min_connections` | entier | `0` | ≤ `max_connections` | Connexions gardées ouvertes même au repos. |
| `acquire_timeout_secs` | entier, s | `5` | > 0 | Temps qu'une requête attend une connexion libre du pool. Au-delà, elle échoue avec `internal server error`. |
| `idle_timeout_secs` | entier, s | `30` | > 0 | Les connexions inactives du pool sont fermées au bout de ce délai. |
| `max_lifetime_secs` | entier, s | `1800` | > 0 | Les connexions du pool sont recyclées au bout de ce délai. |

### Write-behind et mémoire

| Option | Type | Défaut | Validation | Rôle |
| --- | --- | --- | --- | --- |
| `flush_interval_secs` | entier, s | `300` | > 0 | Période du flush par lots des clés sales vers PostgreSQL. C'est aussi la **fenêtre maximale d'écritures perdues en cas de crash**. Voir [durabilité](architecture.md#durability). |
| `pack_idle_secs` | entier, s | `120` | `0` désactive | Un document ni lu ni écrit pendant ce délai est compacté en JSON texte en mémoire (environ 10 fois plus petit que l'arbre parsé). Le prochain accès le reparse sur place, sans PostgreSQL. |
| `idle_eviction_secs` | entier, s | `900` | `0` désactive | Un document **propre** (déjà flushé) ni lu ni écrit pendant ce délai quitte la mémoire. Le prochain accès le recharge avec un `SELECT`. Les documents sales et les documents avec TTL ne sont jamais évincés. |

Les deux options mémoire agissent via le balayage qui tourne **une fois par
minute** : leur résolution effective est donc la minute. Le choix des valeurs
est traité dans le [guide de réglage de la mémoire](operations.md#memory-tuning).

## `[auth]` { #auth }

Un seul compte protège le serveur.

| Option | Type | Défaut | Validation | Rôle |
| --- | --- | --- | --- | --- |
| `username` | chaîne | *obligatoire* | non vide | Identifiant (les espaces autour sont retirés). |
| `password` | chaîne | aucun | exactement un de `password` / `password_hash` | Secret en clair, comparé en temps constant. |
| `password_hash` | chaîne | aucun | exactement un de `password` / `password_hash` | Hash Argon2 au format PHC. Recommandé en production : le secret n'est jamais dans le fichier. |
| `session_timeout_secs` | entier, s | `3600` | > 0 | Une session expire après ce délai **sans activité**. Chaque requête authentifiée la prolonge. |

Renseigner à la fois `password` et `password_hash`, ou aucun des deux (une
chaîne vide ou faite d'espaces compte comme absente), échoue à la validation
avec `auth requires exactly one of password or password_hash`.

### Générer un `password_hash` { #password-hash }

Le serveur lit le hash comme une chaîne PHC et le vérifie avec la crate
[`argon2`](https://docs.rs/argon2). L'algorithme, la version et les paramètres
de coût sont lus dans la chaîne elle-même : toute chaîne PHC Argon2 valide
fonctionne. Prenez **argon2id** :

```text
$argon2id$v=19$m=19456,t=2,p=1$<sel, base64>$<hash, base64>
```

N'importe quel outil qui produit une chaîne PHC standard convient. Deux
classiques :

=== "CLI argon2"

    La CLI de l'implémentation de référence (paquet `argon2` de la plupart
    des distributions). `-k` est le coût mémoire en Kio, `-e` affiche la chaîne
    PHC :

    ```bash
    printf '%s' 'mon-long-secret' | argon2 "$(openssl rand -base64 16)" -id -t 2 -k 19456 -p 1 -e
    ```

=== "Python (argon2-cffi)"

    ```bash
    python3 -c 'from argon2 import PasswordHasher; print(PasswordHasher().hash("mon-long-secret"))'
    ```

Collez ensuite le résultat dans le fichier, entre apostrophes pour que TOML le
garde tel quel :

```toml
[auth]
username = "admin"
password_hash = '$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$…'
```

!!! warning "Un hash mal formé ne se remarque qu'au login"

    Le démarrage vérifie que le champ est présent et non vide, pas qu'il se
    parse. Avec un hash tronqué ou mal recopié, le serveur démarre normalement
    et **chaque** login répond `invalid credentials`. Testez un login juste
    après l'avoir changé.

!!! tip "Paramètres de coût et rafales de logins"

    La vérification Argon2 est coûteuse en CPU, c'est voulu. Le serveur la
    fait tourner sur le pool bloquant de Tokio pour qu'une rafale de logins
    (tous les clients qui se reconnectent après un redémarrage) ne bloque pas
    les autres requêtes, mais chaque login coûte quand même une vérification.
    Le SDK se connecte une fois par connexion, pas par requête.

## Exemple complet commenté { #example }

C'est le `example.config.toml` du dépôt, avec chaque option détaillée :

```toml title="config.toml"
[server]
host = "0.0.0.0"               # (1)!
port = 8080
max_connections = 1000         # 1024 par défaut
# Plus gros message WebSocket (requête ou trame) accepté d'un client, en
# octets. Au-delà, la connexion du client est fermée avec toutes ses requêtes
# en vol. 1 Kio .. 256 Mio, 64 Mio par défaut.
max_message_size = 67108864

[database]
host = "127.0.0.1"
port = 5432
database = "horizon"
username = "horizon"
password = "change-me"
# Pool : utilisé par les lectures à froid et le flush périodique ;
# dimensionnez-le selon le nombre de clients (32 par défaut).
max_connections = 32
min_connections = 0
acquire_timeout_secs = 5
idle_timeout_secs = 30
max_lifetime_secs = 1800
# Fréquence du flush par lots des écritures vers PostgreSQL (secondes).
flush_interval_secs = 300      # (2)!
# Un document flushé et inactif pendant ce délai quitte la mémoire et sera
# rechargé au prochain accès. 0 garde tout en mémoire. 900 par défaut.
idle_eviction_secs = 900
# Un document inactif pendant ce délai est compacté en JSON texte en mémoire,
# reparsé au prochain accès. 0 garde tout parsé. 120 par défaut.
pack_idle_secs = 120

[auth]
username = "admin"
# Exactement un des deux :
#   password      : secret en clair (comparaison en temps constant)
#   password_hash : hash PHC argon2id (recommandé en production)
password = "secret"
# password_hash = "$argon2id$v=19$m=19456,t=2,p=1$..."
session_timeout_secs = 3600
```

1. Le serveur parle `ws://` en clair et n'a pas de TLS. Écoutez sur
   `127.0.0.1` ou une interface privée, ou placez devant un reverse proxy qui
   termine le TLS.
2. Les écritures depuis le dernier flush ne vivent qu'en mémoire. Un crash en
   perd jusqu'à ce nombre de secondes.

## Logs { #logging }

Les logs vont sur la sortie standard et suivent la variable d'environnement
`RUST_LOG` (`info` si elle est absente) :

| Niveau | Ce que vous obtenez |
| --- | --- |
| `info` | Démarrage, les [lignes de stats](operations.md#stats) chaque minute, chaque flush, l'arrêt. |
| `debug` | En plus, chaque requête (opération, table, clé, id, durée), les requêtes refusées et le résultat des balayages. |
| `trace` | En plus, chaque appel au dépôt (`read`, `set`, `delete`, `mutate`…). |

```bash
RUST_LOG=debug ./target/release/horizondb
```

Les erreurs internes (SQL, sérialisation) sont toujours loguées en `error`
avec leurs détails ; les clients ne reçoivent que `internal server error`.
