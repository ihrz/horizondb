---
title: Référence du protocole
description: Le protocole WebSocket de HorizonDB, le format des paquets, chaque opération, les erreurs et les limites.
---

# Référence du protocole

N'importe quel langage qui parle WebSocket et JSON peut être un client
HorizonDB. Cette page décrit le contrat complet sur le fil, tiré du code du
serveur.

!!! note "Travail en cours"

    Le protocole peut encore changer d'une version à l'autre. Gardez le
    serveur et vos clients synchronisés.

## Transport { #transport }

- WebSocket en clair (`ws://hôte:port`). Le serveur n'a **pas de TLS** : gardez-le
  sur un réseau privé, ou derrière un proxy qui termine le TLS pour du `wss://`.
- Un objet JSON par trame **texte**. Les trames binaires sont ignorées.
- Les requêtes d'une connexion sont traitées **en parallèle**. Les réponses
  portent l'`id` de la requête et peuvent arriver dans n'importe quel ordre.
- Jusqu'à 1 024 requêtes peuvent être en vol par connexion. Au-delà, le
  serveur cesse de lire le socket jusqu'à ce que des réponses partent
  (contre-pression TCP).
- Après 30 s sans rien recevoir, le serveur envoie un Ping ; après 120 s de
  silence, il coupe la connexion. Toute trame du client compte comme activité,
  y compris le Pong qu'une bibliothèque WebSocket envoie automatiquement.

## Paquet de requête { #request }

```json
{
  "id": "42",
  "operation": "get",
  "sessionId": "0b7c2a4e-…",
  "table": "json",
  "key": "guild.42.settings.prefix",
  "defaultValue": "!",
  "timeoutMs": 10000
}
```

| Champ | Type | Utilisé par | Notes |
| --- | --- | --- | --- |
| `id` | **chaîne**, obligatoire | toutes | Renvoyé dans la réponse. Doit être une chaîne : un nombre ne passe pas le parsing. |
| `operation` | chaîne, obligatoire | toutes | Une des [opérations](#operations). |
| `sessionId` | chaîne | toutes sauf `login` | Obtenu par `login`. |
| `table` | chaîne | opérations sur les données | `json` par défaut. Doit respecter `[A-Za-z_][A-Za-z0-9_]{0,62}`. |
| `key` | chaîne | toutes sauf `all` et `deleteAll` | Clé de base plus un chemin pointé optionnel, voir [clés](#keys). |
| `value` | JSON quelconque | `set`, `cache` | Absent signifie `null`. |
| `amount` | nombre | `add`, `sub` | `1` par défaut. Doit être fini. |
| `element` | JSON quelconque | `push`, `pull` | Absent signifie `null`. |
| `defaultValue` | JSON quelconque | `get` | Renvoyé quand la clé ou le chemin ne mène à rien. |
| `time` | entier, ms | `cache` | TTL. `60000` par défaut, de 1 ms à 30 jours. |
| `returnValue` | booléen | `push`, `pull` | Si `true`, la réponse contient le tableau obtenu. |
| `login`, `password` | chaîne | `login` | Identifiants de `[auth]`. |
| `timeoutMs` | entier, ms | toutes | Timeout du client, voir [échéances](#deadlines). |

Les champs inconnus sont ignorés. Un champ du mauvais type (par exemple un
`time` négatif) rend tout le paquet invalide.

## Paquet de réponse { #response }

Chaque réponse a les mêmes quatre champs ; ceux qui ne servent pas valent
`null`.

=== "Succès"

    ```json
    { "id": "42", "type": "response", "data": "!", "error": null }
    ```

=== "Erreur"

    ```json
    { "id": "42", "type": "error", "data": null, "error": "invalid or expired session" }
    ```

=== "Requête illisible"

    ```json
    { "id": "unknown", "type": "error", "data": null, "error": "Invalid JSON: missing field `operation` at line 1 column …" }
    ```

    Quand le paquet lui-même ne se parse pas (pas du JSON, `id` ou
    `operation` manquant, champ du mauvais type), le serveur ne peut pas
    connaître l'`id` : il répond avec `"id": "unknown"`. Une telle requête
    n'est pas comptée dans les stats.

## Sessions { #sessions }

1. Envoyez `login` avec les identifiants. La réponse contient un `sessionId`.
2. Mettez ce `sessionId` dans chaque autre requête.
3. La session reste valide tant qu'elle sert : chaque requête authentifiée la
   prolonge, et elle expire après `auth.session_timeout_secs` (3 600 s par
   défaut) **sans** activité.
4. Une session n'est pas liée à la connexion qui l'a créée.
5. Les sessions ne vivent qu'en mémoire : après un redémarrage du serveur,
   chaque requête répond `invalid or expired session` jusqu'à ce que le client
   se reconnecte.

## Clés et imbrication { #keys }

Une clé est découpée sur les `.` : le premier segment est la **clé de base**
(un document stocké), le reste est un **chemin** dans ce document.

| Règle | Erreur si elle n'est pas respectée |
| --- | --- |
| Présente et non vide | `key is required` |
| Aucun segment vide (`.a`, `a.`, `a..b`) | `malformed key` |
| 32 segments au plus | `key nesting too deep` |
| 255 octets au plus, clé pointée entière | `key too long` |

Les segments de chemin sont toujours des clés d'objet : `list.0` désigne la
propriété `"0"`, pas le premier élément d'un tableau.

Comment les écritures imbriquées traitent l'existant :

- Les objets intermédiaires manquants sont créés.
- Si la valeur **juste au-dessus** de la feuille existe et n'est pas un objet,
  l'écriture échoue avec `cannot set nested value on non-object`.
- Une valeur non objet trouvée **plus haut** dans le chemin est remplacée par
  un objet.

```text
document {"a": 5}
set   a.b    →  erreur : cannot set nested value on non-object
set   a.b.c  →  {"a": {"b": {"c": …}}}
```

## Opérations { #operations }

Les exemples omettent `sessionId` et `table` pour rester lisibles.

### `login` { #op-login }

Ouvre une session. Pas besoin de `sessionId`.

```js
/* → */ { "id": "1", "operation": "login", "login": "admin", "password": "secret" }
/* ← */ { "id": "1", "type": "response", "data": { "success": true, "sessionId": "0b7c2a4e-…" }, "error": null }
```

De mauvais identifiants donnent `invalid credentials`.

### `logout` { #op-logout }

Révoque le `sessionId` fourni. Réussit toujours, même sans session.

```js
/* → */ { "id": "2", "operation": "logout", "sessionId": "0b7c2a4e-…" }
/* ← */ { "id": "2", "type": "response", "data": { "success": true }, "error": null }
```

### `get` { #op-get }

Lit un document entier ou le sous-arbre d'un chemin. Seule la partie demandée
est copiée.

```js
/* → */ { "id": "3", "operation": "get", "key": "guild.42.settings" }
/* ← */ { "id": "3", "type": "response", "data": { "prefix": "!" }, "error": null }

/* → */ { "id": "4", "operation": "get", "key": "guild.42.missing", "defaultValue": 0 }
/* ← */ { "id": "4", "type": "response", "data": 0, "error": null }
```

`data` vaut la valeur, `defaultValue` quand la clé ou le chemin ne mène à rien,
ou `null` s'il n'y a pas de valeur par défaut.

### `set` { #op-set }

Écrit une valeur.

- Clé de premier niveau : remplace le document entier (et retire un éventuel
  TTL).
- Clé imbriquée : fusionne la valeur au chemin indiqué, en créant les objets
  intermédiaires.

```js
/* → */ { "id": "5", "operation": "set", "key": "guild.42.settings.prefix", "value": "?" }
/* ← */ { "id": "5", "type": "response", "data": { "success": true }, "error": null }
```

### `delete` { #op-delete }

Supprime un document entier (clé de premier niveau) ou une feuille (clé
imbriquée). `existed` indique s'il y avait quelque chose à supprimer.

```js
/* → */ { "id": "6", "operation": "delete", "key": "guild.42.settings.prefix" }
/* ← */ { "id": "6", "type": "response", "data": { "success": true, "existed": true }, "error": null }
```

Quand une suppression imbriquée laisse le document vide (`{}`), le document
entier est supprimé.

### `add` / `sub` { #op-add }

Ajoute ou retire `amount` (1 par défaut) et répond avec le nouveau nombre. Une
clé ou un chemin absent part de 0 : `sub` sur une clé absente donne `-amount`.

```js
/* → */ { "id": "7", "operation": "add", "key": "guild.42.stats.messages", "amount": 5 }
/* ← */ { "id": "7", "type": "response", "data": { "success": true, "value": 5.0 }, "error": null }
```

- La cible doit être un nombre : tout le reste (y compris un `null` stocké)
  échoue avec `cannot add to non-numeric value`.
- `amount` doit être fini (`amount must be finite`).
- Les nombres sont des flottants 64 bits : le résultat est écrit comme un
  flottant (`5.0`), et les entiers au-delà de 2^53 perdent en précision.

### `push` { #op-push }

Ajoute `element` à la fin d'un tableau.

- Clé de premier niveau : le document doit être un tableau ; s'il manque, il
  part de `[]`.
- Clé imbriquée : un document manquant part de `{}` et un tableau manquant au
  chemin est créé.
- Une cible existante qui n'est pas un tableau échoue avec
  `target value is not an array`.

```js
/* → */ { "id": "8", "operation": "push", "key": "guild.42.mods", "element": "1234", "returnValue": true }
/* ← */ { "id": "8", "type": "response", "data": { "success": true, "value": ["1234"] }, "error": null }
```

Sans `returnValue`, `data` vaut `{ "success": true }`.

### `pull` { #op-pull }

Retire **tous** les éléments égaux à `element` (égalité JSON profonde). Même
option `returnValue` et mêmes règles que `push`.

```js
/* → */ { "id": "9", "operation": "pull", "key": "guild.42.mods", "element": "1234", "returnValue": true }
/* ← */ { "id": "9", "type": "response", "data": { "success": true, "value": [] }, "error": null }
```

!!! note "Un `pull` sur un tableau absent le crée"

    Comme `push`, `pull` crée le tableau quand il n'existe pas : un `pull` sur
    une clé absente stocke un tableau vide.

### `has` { #op-has }

Indique si la clé ou le chemin contient une valeur. Un `null` stocké compte
comme absent. `data` est un booléen nu.

```js
/* → */ { "id": "10", "operation": "has", "key": "guild.42.mods" }
/* ← */ { "id": "10", "type": "response", "data": true, "error": null }
```

### `all` { #op-all }

Renvoie les documents de la table sous forme de paires `{ id, value }` triées
par `id`. `key` est ignorée.

```js
/* → */ { "id": "11", "operation": "all", "table": "json" }
/* ← */ { "id": "11", "type": "response", "data": [ { "id": "guild", "value": { "42": { "settings": { "prefix": "!" } } } } ], "error": null }
```

Le résultat fusionne les lignes PostgreSQL avec les écritures pas encore
flushées, et s'arrête à **10 000 lignes**. Ce n'est pas une API de pagination :
une table plus grande est tronquée.

### `deleteAll` { #op-deleteall }

Vide la table : la mémoire tout de suite, PostgreSQL (`DELETE FROM`) au
prochain flush. La table elle-même est conservée.

```js
/* → */ { "id": "12", "operation": "deleteAll", "table": "sessions_cache" }
/* ← */ { "id": "12", "type": "response", "data": { "success": true }, "error": null }
```

### `cache` { #op-cache }

Écrit une valeur avec une durée de vie, `time` en millisecondes (60 000 par
défaut, 30 jours au plus). Une clé expirée se lit comme absente immédiatement,
puis est retirée de la mémoire, et de PostgreSQL au prochain flush.

```js
/* → */ { "id": "13", "operation": "cache", "key": "cooldown.42.daily", "value": true, "time": 86400000 }
/* ← */ { "id": "13", "type": "response", "data": { "success": true }, "error": null }
```

!!! warning "Un TTL s'applique au document de base entier"

    Avec une clé imbriquée, la valeur est fusionnée au chemin, mais le TTL est
    posé sur le **document de base** : `cache("temp.token", …)` supprime tout
    `temp` à l'expiration. Toute écriture ordinaire ultérieure n'importe où
    dans ce document (`set`, `add`, `push`…) le rend de nouveau permanent.
    L'échéance n'existe qu'en mémoire : une clé mise en cache et déjà flushée
    revient sans TTL si le serveur redémarre avant son expiration.

## Échéances { #deadlines }

Un paquet peut porter `timeoutMs`, le timeout propre du client. Avant
d'exécuter une requête, le serveur le compare au temps qu'elle a passé à
attendre depuis sa lecture (derrière la limite de requêtes en vol, par
exemple). Si elle a attendu plus longtemps, le serveur répond :

```json
{ "id": "14", "type": "error", "data": null, "error": "deadline exceeded" }
```

et ne travaille pas pour un appelant qui a déjà abandonné. Sans `timeoutMs`
(ou avec `0`), l'échéance est de 10 s. La vérification n'a lieu qu'avant
l'exécution : une requête commencée va toujours jusqu'au bout.

## Erreurs { #errors }

Les messages d'erreur sont des chaînes stables, destinées aux programmes autant
qu'aux humains. Ils restent en anglais sur le fil.

| Message | Cause |
| --- | --- |
| `invalid credentials` | `login` avec un mauvais identifiant ou mot de passe. |
| `invalid or expired session` | `sessionId` absent, inconnu, révoqué ou expiré (y compris après un redémarrage du serveur). |
| `Unknown operation: <nom>` | `operation` ne fait pas partie de la liste (vérifié après la session). |
| `key is required` | `key` absente ou vide. |
| `malformed key` | Segment vide dans la clé. |
| `key nesting too deep` | Plus de 32 segments. |
| `key too long` | Clé de plus de 255 octets. |
| `table name must match [A-Za-z_][A-Za-z0-9_]{0,62}` | `table` invalide. |
| `amount must be finite` | `amount` vaut NaN ou l'infini. |
| `cannot add to non-numeric value` | `add` / `sub` sur une valeur qui n'est pas un nombre. |
| `cannot set nested value on non-object` | Écriture imbriquée sous une valeur qui n'est pas un objet. |
| `target value is not an array` | `push` / `pull` sur autre chose qu'un tableau. |
| `ttl must be between 1ms and 30 days` | `cache` avec `time` = 0 ou au-delà de 30 jours. |
| `deadline exceeded` | La requête a attendu plus que son `timeoutMs` avant de s'exécuter. |
| `Invalid JSON: <détail>` | Le paquet n'a pas pu être parsé (réponse avec `id: "unknown"`). |
| `internal server error` | Échec PostgreSQL ou de sérialisation. Les détails ne sont que dans le log du serveur, en `error`. |

## Limites { #limits }

| Limite | Valeur | Au-delà |
| --- | --- | --- |
| Taille des messages et trames | `server.max_message_size`, 64 Mio par défaut, 256 Mio au plus | La connexion est fermée, avec toutes ses requêtes en vol. Logué en `warn`. |
| Connexions | `server.max_connections`, 1 024 par défaut | Les nouvelles connexions sont fermées. |
| Requêtes en vol par connexion | 1 024 | Le serveur cesse de lire le socket (contre-pression). |
| Échéance d'une requête | `timeoutMs`, 10 s par défaut | `deadline exceeded`. |
| Longueur de clé | 255 octets | `key too long`. |
| Profondeur de clé | 32 segments | `key nesting too deep`. |
| Nom de table | 63 octets, `[A-Za-z_][A-Za-z0-9_]*` | Erreur de validation. |
| Résultat de `all` | 10 000 lignes (`MAX_LIST_ROWS`) | Tronqué. |
| TTL | de 1 ms à 30 jours | Erreur de validation. |
| Connexion silencieuse | Ping après 30 s, fermeture après 120 s | Connexion fermée. |
| Session | `auth.session_timeout_secs` d'inactivité | `invalid or expired session`. |
