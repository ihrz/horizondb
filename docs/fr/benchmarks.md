---
title: Benchmarks
description: Comment HorizonDB est mesuré, les chiffres de référence, les mesures mémoire, et comment les reproduire.
---

# Benchmarks

<div class="grid cards" markdown>

-   :material-chart-line:{ .lg .middle } **Résultats interactifs**

    ---

    Chaque scénario, les percentiles de latence et les mesures mémoire, en
    graphiques, à partir des derniers résultats publiés.

    [:octicons-arrow-right-24: Ouvrir la page des benchmarks](https://ihrz.gitlab.io/db/benchmarks/){ .md-button .md-button--primary }

</div>

## Méthode { #method }

La suite (`benchmarks/bench.ts`) passe **de bout en bout par le SDK
TypeScript officiel**, contre un vrai serveur et un vrai PostgreSQL : aucun
mock, chaque chiffre inclut le réseau, le framing WebSocket, le serveur et,
quand il intervient, PostgreSQL.

- Un pool de connexions clientes (16 par défaut), chacune en pipeline
  (32 requêtes en vol par connexion par défaut).
- Les clés de charge utilisent des **clés de base uniques** pour modéliser un
  trafic clé-valeur ordinaire ; seuls les scénarios de contention visent
  volontairement un document partagé.
- **Borné dans le temps.** Chaque scénario a un budget de temps réel (`--cap`,
  25 s par défaut) vérifié entre les opérations : la suite se termine toujours.
- **Verdict binaire.** Le code de sortie vaut 1 à la moindre erreur, et les
  scénarios de contention vérifient un invariant (compteur final == nombre
  d'incréments) : une seule mise à jour perdue fait échouer la passe.

### Scénarios { #scenarios }

| Scénario | Opération(s) | Accès | Ce qu'il mesure |
| --- | --- | --- | --- |
| `get-hot/sequential` | `get` | 1 connexion, clé chaude | Latence plancher : aller-retour + mémoire |
| `has/sequential` | `has` | 1 connexion, clé chaude | Idem, chemin d'existence |
| `set-small/sequential` | `set` | 1 connexion, clés uniques | Débit d'écriture sans concurrence |
| `set-small/pipelined` | `set` | 16 connexions × profondeur 32 | Débit d'écriture saturé |
| `add-counter/contented` | `add` | 16 workers sur **une seule clé** | Sérialisation par clé sous contention maximale, avec l'invariant de somme |
| `nested-merge/contented` | `add` imbriqué | 16 workers sur un document | Modifications imbriquées sur place sous contention, avec l'invariant |
| `mixed-workload/pipelined` | get/set/add/has/delete | 512 requêtes en vol | Charge mixte réaliste |
| `payload-Nkb/set+get` | `set` + `get` | 1 connexion, gros documents | Effet de la taille (1 / 64 / 512 Kio selon le profil) |
| `all-scan/preloaded` | `all` | Table préchargée | Coût d'un parcours complet (10 000 lignes au plus) |
| `session-churn/login+logout` | `login` / `logout` | Boucle serrée | Coût pur de l'authentification |
| `spike-burst/idle->flood` | `set` | 250 ms de repos, puis un déluge | Absorption d'une rafale après une pause |

### Lire une ligne de résultat { #reading }

```text
scenario                            ops  err     ops/s    avgµs    p50µs    p95µs    p99µs    maxµs
get-hot/sequential                 2.0k    0     18.1k       45       42       67       74      142
```

- **ops** : opérations mesurées (échantillonnage plafonné à 500 k).
- **err** : erreurs côté client (timeout, fermeture, refus du serveur). Doit
  valoir 0.
- **ops/s** : débit moyen sur la durée réelle du scénario.
- **avg / p50 / p95 / p99 / max** : latence par opération en µs, attente dans
  le pipeline comprise.

Un p50 bas avec un p99 haut signale une traîne de latence : contention ou
requêtes proches de leur timeout. Une durée proche du budget indique un
scénario coupé par son plafond.

## Résultats de référence { #results }

Tirés de `benchmarks/BENCHMARKS.md` : portable Linux, NVMe grand public,
PostgreSQL 16 en conteneur (réseau de l'hôte, `synchronous_commit=off`),
Bun 1.4, **build debug** du serveur, profil `--quick`, 16 connexions,
pipeline 32.

| Chemin | Débit | p50 |
| --- | --- | --- |
| `get` chaud, une connexion | 18,1 k ops/s | 42 µs |
| `has`, une connexion | 20,1 k ops/s | 39 µs |
| login + logout | 17,2 k ops/s | 43 µs |
| `add` sur une clé, 16 workers | 1,9 k ops/s | 545 µs |
| `set` + `get`, documents de 1 Kio | 1,3 k ops/s | 784 µs |
| `set` + `get`, documents de 64 Kio | 311 ops/s | 3 059 µs |
| client WebSocket brut, même serveur | 48 k ops/s | — |

La page interactive montre la dernière passe complète, y compris les
résultats du build release.

!!! info "Problème connu : `set` simple via le SDK dans les longues passes mixtes"

    `set-small/pipelined`, `mixed-workload/pipelined`, `set-small/sequential`
    et `spike-burst` se dégradent dans les longues passes via le SDK Bun, alors
    que le serveur répond à chaque requête en moins de 10 ms avec 100 % de
    succès, et qu'un client WebSocket brut atteint 48 k ops/s avec la même
    forme de charge. L'interaction entre le client WebSocket de Bun et la
    fenêtre TCP est encore à l'étude. Contournements : `--pipeline 4`, ou des
    scénarios lancés séparément avec `BENCH_SKIP`.

## Mémoire { #memory }

La politique mémoire (compactage et éviction, voir
[niveaux de mémoire](architecture.md#memory-tiers)) a été mesurée avec un build
release sur **200 000 documents d'environ 1 Ko (185 Mo de JSON)**, des objets
façon guilde avec réglages, membres, ids de rôles et blocs imbriqués.
Déroulé : tout charger, flusher, 80 s sans trafic, relire 2 000 clés au hasard,
réécrire 10 %, redémarrer, relire. Chaque lecture a été comparée au document
généré : **zéro différence** dans toutes les configurations, avant et après le
redémarrage.

Mémoire résidente (RSS) après 80 s de repos :

| Configuration | RSS au repos |
| --- | --- |
| branche main, sans politique | **2 703 Mio**, et elle y reste |
| éviction seule | **60 Mio** |
| compactage seul (tous les documents restent en mémoire) | **417 Mio** |
| compactage + éviction | **360 Mio** |

Ces passes utilisaient des seuils courts (30 à 60 s) pour tenir dans le
déroulé ; les valeurs par défaut sont 120 s pour le compactage et 900 s pour
l'éviction.

Coût en mémoire d'un document d'environ 940 octets de JSON :

| Forme | Octets par document |
| --- | --- |
| arbre `serde_json` parsé (forme chaude) | ~11 200 |
| JSON texte compacté (forme au repos) | ~1 000 |

Reparser un document compacté : environ **4 µs** pour un document de 1 Ko,
environ **7 ms** pour un document de 870 Ko.

## Reproduire { #reproduce }

Prérequis : Bun ≥ 1.4, Docker, une toolchain Rust.

```bash
./scripts/dev-up.sh                  # conteneur PostgreSQL, config.toml, build, serveur détaché
./scripts/bench.sh                   # suite standard (~2 min, bornée)
./scripts/bench.sh --quick          # budgets ×0,25 (comme --profile quick)
./scripts/bench.sh --full           # budgets ×3, documents jusqu'à 512 Kio
```

`bench.sh` appelle lui-même `dev-up.sh`, le serveur et sa base sont donc
toujours prêts. Attention, `dev-up.sh` compile un binaire **debug** ; pour des
chiffres release, lancez un serveur release et la suite directement contre lui :
`bun benchmarks/bench.ts --url ws://hôte:port`.

| Option | Défaut | Rôle |
| --- | --- | --- |
| `--profile <quick\|standard\|full>` | `standard` | Échelle des budgets (×0,25 / ×1 / ×3). |
| `--concurrency N` | `16` | Connexions WebSocket du pool client. |
| `--pipeline N` | `32` | Requêtes en vol par connexion. |
| `--timeout ms` | `10000` | Timeout par requête. |
| `--cap ms` | `25000` | Budget de temps réel par scénario. |
| `--url`, `--user`, `--pass`, `--table` | env / local | Cible. Environnement : `HORIZON_URL`, `HORIZON_USER`, `HORIZON_PASS`. |
| `--json <chemin>` | aucun | Écrit aussi les résultats en JSON dans `chemin`. |

`BENCH_SKIP` prend une liste de scénarios à sauter, séparés par des virgules
(par exemple `BENCH_SKIP="spike-burst/idle->flood"`). `BENCH_LABEL` nomme la
machine dans la sortie JSON.

### Publier des résultats { #publishing }

La page interactive lit deux fichiers du dépôt :

- `benchmarks/results/latest.json` : la sortie de `--json`, avec les réglages,
  l'environnement (version de Bun, plateforme, `BENCH_LABEL`) et chaque
  scénario.
- `benchmarks/results/memory.json` : les mesures mémoire ci-dessus.

```bash
BENCH_LABEL="mon portable, build release" bun benchmarks/bench.ts --json benchmarks/results/latest.json
```

### En CI { #ci }

```yaml
- run: ./scripts/test.sh                   # tests unitaires (Rust + SDK)
- run: ./scripts/bench.sh --profile quick  # smoke test de perf : échoue sur erreur ou invariant cassé
```
