---
title: Exploitation
description: Déployer, arrêter, surveiller, régler la mémoire, dimensionner PostgreSQL, mettre à jour et sauvegarder HorizonDB.
---

# Exploitation

## Déploiement { #deploy }

Compilez un binaire release sur la cible (ou sur une machine avec la même
libc) :

```bash
cargo build --release      # target/release/horizondb
```

Le processus a besoin : d'un accès en lecture à son `config.toml`, d'un accès
réseau à PostgreSQL, et de son port d'écoute. Il n'écrit rien sur le disque en
dehors de ses logs sur la sortie standard.

### systemd { #systemd }

Le dépôt fournit une unité durcie, [`horizondb.service`](https://gitlab.com/ihrz/db/-/blob/main/horizondb.service) :

```ini title="/etc/systemd/system/horizondb.service"
[Unit]
Description=HorizonDB key-value database server
Documentation=https://gitlab.com/ihrz/db
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=ihrz
Group=ihrz
WorkingDirectory=/home/ihrz/db
ExecStart=/home/ihrz/db/target/release/horizondb /home/ihrz/db/config.toml
Restart=on-failure
RestartSec=2
# The final flush serializes every dirty document; give it time on busy
# deployments instead of letting systemd SIGKILL it (writes since the last
# flush would be lost).
TimeoutStopSec=300

# SIGTERM triggers a graceful shutdown that flushes pending writes to PostgreSQL.
# Logging defaults to "info"; use RUST_LOG=debug|trace for more detail.
# Environment=RUST_LOG=info

# Hardening (safe for a network-only server: reads config, writes to PostgreSQL).
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
```

Adaptez `User`, `Group`, `WorkingDirectory` et les chemins, puis :

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now horizondb
journalctl -u horizondb -f
```

`ProtectHome=read-only` suffit car le serveur ne fait que lire dans le
répertoire personnel. Si PostgreSQL tourne sur la même machine, ajoutez
`After=postgresql.service` pour que la base soit prête avant la première
tentative de connexion (le serveur s'arrête s'il ne peut pas se connecter au
démarrage, et `Restart=on-failure` réessaie toutes les 2 s).

### pm2 { #pm2 }

pm2 peut superviser le binaire directement. Donnez-lui un délai d'arrêt
supérieur au budget d'arrêt de 10 s du serveur, sinon la valeur par défaut de
pm2 (1,6 s) envoie un SIGKILL en plein flush final :

```js title="ecosystem.config.cjs"
module.exports = {
  apps: [
    {
      name: "horizondb",
      script: "./target/release/horizondb",
      args: "config.toml",
      cwd: "/home/ihrz/db",
      interpreter: "none",
      kill_timeout: 15000,
      env: { RUST_LOG: "info" },
    },
  ],
};
```

```bash
pm2 start ecosystem.config.cjs
pm2 logs horizondb
```

## Arrêt propre { #shutdown }

SIGINT et SIGTERM déclenchent la même séquence :

1. Log `shutdown signal received` et **fermeture du socket d'écoute** : le port
   est libre tout de suite pour l'instance suivante, même si le flush est
   lent.
2. Arrêt des boucles de fond (purge des sessions, balayage mémoire, stats).
3. Fermeture de toutes les connexions clientes, pour qu'aucune nouvelle
   écriture ne croise le flush final.
4. Log des dernières lignes de stats.
5. Arrêt de la boucle de flush, **flush final** de toutes les clés sales,
   fermeture du pool PostgreSQL.
6. Log `HorizonDB stopped` et sortie.

Les étapes 4 et 5 tiennent dans un budget de **10 secondes**. Si PostgreSQL
est bloqué ou si le flush final est trop gros, le serveur logue
`graceful shutdown timed out; buffered writes may not have reached PostgreSQL`
et quitte quand même : une base bloquée ne peut jamais garder le processus en
vie.

!!! warning "Le budget de 10 s est la vraie limite"

    `TimeoutStopSec=300` dans l'unité empêche seulement systemd de tuer le
    processus trop tôt ; le serveur, lui, abandonne au bout de 10 s. Sur une
    instance chargée, gardez le volume de données sales assez petit pour être
    flushé dans ce délai : un `flush_interval_secs` plus court en laisse moins
    à écrire à l'arrêt. Surveillez le champ `elapsed` des lignes
    `repository flush complete` pour savoir où vous en êtes.

Le serveur quitte ensuite sèchement au lieu de démonter le runtime, pour
qu'une tâche bloquée ne laisse pas un processus à moitié mort qui tient le
port. Au démarrage, il réessaie un port occupé pendant 15 s
(`port in use, retrying…`), ce qui couvre le chevauchement avec l'instance
précédente lors d'un `systemctl restart` ou d'un `pm2 restart`.

## Surveillance { #monitoring }

### Les lignes de stats { #stats }

Chaque minute (et une dernière fois à l'arrêt), le serveur logue deux lignes
`info` :

```text
INFO server stats active_connections=… total_connections=… active_sessions=… total_requests=… success_rate="…%" memory_hit_rate="…%" memory_entries=… memory_evicted=… memory_packed=…
INFO memory stats allocated="… MiB" resident="… MiB"
```

| Champ | Signification |
| --- | --- |
| `active_connections` | Connexions WebSocket ouvertes en ce moment. |
| `total_connections` | Connexions TCP acceptées depuis le démarrage, y compris celles refusées par `max_connections`. |
| `active_sessions` | Sessions dans le stockage. Les sessions expirées comptent jusqu'à la purge, toutes les 5 minutes. |
| `total_requests` | Requêtes traitées depuis le démarrage, y compris celles qui ont reçu `deadline exceeded`. Les paquets illisibles ne comptent pas. |
| `success_rate` | Part de ces requêtes qui n'ont pas répondu une erreur. Erreurs de validation, logins refusés, sessions expirées et échéances comptent comme des échecs. `100.00%` avant la première requête. |
| `memory_hit_rate` | Part des recherches en mémoire qui ont trouvé la clé, depuis le démarrage. Un échec signifie une clé absente de la mémoire : chargement à froid, rechargement après éviction, ou clé inexistante. Chaque mutation fait deux recherches. `0.00%` avant la première recherche. |
| `memory_entries` | Documents en mémoire (chauds et compactés) après le dernier balayage. |
| `memory_packed` | Parmi eux, les documents compactés après le dernier balayage. |
| `memory_evicted` | Documents évincés depuis le démarrage (cumul). |
| `allocated` | Octets que l'allocateur (jemalloc) fournit actuellement au serveur. |
| `resident` | Mémoire physique mappée par l'allocateur, le chiffre le plus proche du RSS du processus. |

La ligne `memory stats` est absente des builds MSVC (Windows), qui n'utilisent
pas jemalloc.

### Autres lignes à surveiller { #log-lines }

| Niveau | Message | Signification |
| --- | --- | --- |
| `info` | `repository flush complete` | Un flush, avec `cleared`, `deletes`, `upserts`, `statements` et `elapsed`. |
| `error` | `repository flush failed, will retry next cycle` | PostgreSQL a refusé ou perdu une requête du flush. Les données restent sales en mémoire. |
| `error` | `internal request failure` | Une requête a échoué côté PostgreSQL ou sérialisation ; le client a reçu `internal server error`. |
| `warn` | `connection limit reached, rejecting` | `server.max_connections` est atteint. |
| `warn` | `connection ended with error` | Un lien client a cassé : message trop gros (erreur `Capacity`), inactivité, erreur réseau. |
| `warn` | `request dropped: deadline exceeded before dispatch` | Une requête a attendu au-delà de son `timeoutMs`. Surcharge, ou timeout client trop court. |
| `error` | `graceful shutdown timed out; …` | Le flush final n'a pas fini en 10 s. |

## Réglage de la mémoire { #memory-tuning }

Deux options décident du coût mémoire des documents inactifs, toutes deux
appliquées par le balayage qui tourne une fois par minute :

- `pack_idle_secs` (120 par défaut) compacte les documents inactifs en JSON
  texte, environ dix fois plus petit que l'arbre parsé, sans quitter la
  mémoire. Les reparser coûte des microsecondes (environ 4 µs pour un document
  de 1 Ko, 7 ms pour 870 Ko) et ne touche jamais PostgreSQL. Laissez-le actif.
- `idle_eviction_secs` (900 par défaut) retire de la mémoire les documents
  **propres** inactifs ; l'accès suivant paie un `SELECT`. C'est ce qui borne
  la mémoire aux données actives.

Une méthode pour `idle_eviction_secs` :

1. **Commencez à `1800`.**
2. Laissez tourner le serveur une journée normale et lisez `memory_hit_rate`
   sur la ligne de stats.
3. S'il reste **au-dessus de 97–98 %**, presque tout est servi depuis la
   mémoire : descendez à **900** pour économiser davantage.
4. S'il passe **sous 95 %**, trop d'accès attendent PostgreSQL : montez à
   **3600**.
5. Regardez `memory_entries`, `memory_evicted` et `resident` évoluer en
   conséquence.

À garder en tête :

- Des valeurs **inférieures à `flush_interval_secs`** (300 s par défaut)
  n'évincent pas plus tôt : un document écrit reste sale, donc en mémoire,
  jusqu'au prochain flush. Seuls les documents simplement lus partent plus
  tôt.
- Le balayage tourne chaque minute : une précision inférieure à la minute ne
  change rien.
- `0` désactive une étape. `idle_eviction_secs = 0` ramène l'ancien
  comportement : tout document lu reste en mémoire jusqu'au redémarrage.
- Les clés écrites avec `cache` (TTL) ne sont jamais évincées, seulement
  compactées puis purgées à leur expiration.

## Dimensionner PostgreSQL { #postgresql }

- **Connexions.** Le serveur ouvre au plus `database.max_connections` (32 par
  défaut). Le `max_connections` de PostgreSQL doit laisser de la place pour
  lui et pour vos autres clients. Le flush utilise une connexion à la fois ;
  le pool sert surtout aux **lectures à froid**, qui culminent juste après un
  redémarrage, quand chaque client lit des clés pas encore en mémoire. Une
  requête qui attend une connexion plus de `acquire_timeout_secs` échoue avec
  `internal server error`.
- **Volume d'écriture.** Chaque flush réécrit le **document entier** de chaque
  clé sale (`value` est remplacée, pas patchée). Un gros document écrit une
  fois par intervalle coûte sa taille complète à chaque flush. PostgreSQL
  stocke les grosses lignes en TOAST, et les lignes réécrites laissent des
  tuples morts : gardez l'autovacuum actif et vérifiez qu'il suit sur les
  tables les plus actives.
- **Latence de commit.** Le conteneur de développement tourne avec
  `synchronous_commit=off` ; en production, gardez les réglages de durabilité
  de PostgreSQL et laissez le flush par lots absorber le coût (un commit par
  requête de 500 lignes au plus).
- **TLS.** La configuration n'a pas d'option TLS ; le comportement par défaut
  du driver (`prefer` : TLS si le serveur le propose) s'applique.

## Mise à jour { #upgrading }

1. Compilez le nouveau binaire.
2. Redémarrez le service (`systemctl restart horizondb` ou
   `pm2 restart horizondb`). L'ancien processus flushe et quitte, le nouveau
   prend le port dès qu'il est libre.
3. Les clients voient leurs connexions se fermer et leurs sessions disparaître ;
   le SDK se reconnecte et refait le login tout seul.

Le protocole évolue encore : mettez à jour le SDK dans vos applications en même
temps que le serveur, idéalement épinglés sur le même tag git.

## Sauvegardes { #backups }

Les données sont dans des tables PostgreSQL ordinaires, une par table
HorizonDB :

```sql
CREATE TABLE "<table>" (id VARCHAR(255) PRIMARY KEY, value TEXT);
```

`value` est du JSON texte, qu'on peut interroger avec un cast :

```sql
SELECT id, value::jsonb -> '42' -> 'settings' AS settings FROM "json" WHERE id = 'guild';
```

Sauvegardez avec vos outils habituels (`pg_dump`, sauvegardes de base,
archivage des WAL). Deux points à retenir :

- **Une sauvegarde contient l'état du dernier flush.** Les écritures des
  dernières `flush_interval_secs` sont encore en mémoire. Pour une sauvegarde
  complète, arrêtez d'abord le serveur proprement (il flushe), puis faites le
  dump.
- **Restaurez serveur arrêté.** Un serveur en marche continue de servir ses
  copies en mémoire et écraserait les lignes restaurées avec ses données
  sales au prochain flush. Arrêtez-le, restaurez, redémarrez-le : la mémoire
  repart vide et les documents se chargent à la demande depuis les tables
  restaurées.
