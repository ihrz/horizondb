---
title: HorizonDB
description: Serveur clé-valeur JSON sur WebSocket. La mémoire au-dessus, PostgreSQL en dessous.
hide:
  - navigation
  - toc
---

<div class="hz-hero" markdown>

![HorizonDB](assets/logo.svg)

<span class="hz-eyebrow">Clé-valeur JSON sur WebSocket</span>

# HorizonDB

<p class="hz-tagline">La mémoire au-dessus, PostgreSQL en dessous.</p>

<p class="hz-lead">Le serveur clé-valeur derrière iHorizon. Lectures et écritures à la vitesse de la mémoire, versées dans PostgreSQL par lots en coulisses, le tout sur un seul WebSocket depuis un client Bun sans dépendance.</p>

[Démarrer](getting-started.md){ .md-button .md-button--primary }
[Référence du protocole](protocol.md){ .md-button }
[Résultats des benchmarks](https://ihrz.gitlab.io/db/benchmarks/){ .md-button }

</div>

## Ce que c'est

HorizonDB est un **serveur de base de données clé-valeur orienté JSON**, écrit
en Rust (édition 2024, Tokio async, `unsafe` interdit). Les clients lui parlent
sur un WebSocket, avec de petits paquets JSON. Chaque lecture et chaque
écriture est servie depuis un stockage en mémoire ; une tâche de fond écrit les
clés modifiées dans PostgreSQL par lots toutes les `flush_interval_secs`
(300 s par défaut), et un arrêt propre vide tout d'abord.

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

## Pourquoi

Un bot Discord martèle quelques gros documents JSON (une guilde, un membre, un
compteur) depuis de nombreux shards en même temps. Un aller-retour SQL à chaque
`add` ou `push`, c'est lent ; un stockage purement en mémoire oublie tout au
redémarrage. HorizonDB se tient sur la ligne entre les deux : les données
actives vivent en mémoire, la copie durable dans des tables PostgreSQL
ordinaires que vous savez déjà interroger, sauvegarder et restaurer.

## Points forts

<div class="grid cards" markdown>

-   :material-white-balance-sunny:{ .lg .middle } **La mémoire au-dessus**

    ---

    Chaque opération répond depuis la mémoire. Un `push` dans un document de
    plusieurs mégaoctets le modifie sur place sous un verrou par clé : aucun
    document n'est copié en entier sur le chemin chaud.

-   :material-weather-sunset-down:{ .lg .middle } **L'horizon**

    ---

    Les clés sales descendent dans PostgreSQL par lots (upserts `UNNEST`,
    suppressions `ANY`) toutes les `flush_interval_secs`, et à SIGINT / SIGTERM.

-   :material-database-outline:{ .lg .middle } **PostgreSQL en dessous**

    ---

    Une table simple par espace de noms : `id VARCHAR(255) PRIMARY KEY, value TEXT`.
    Les clés se chargent à la demande, en single-flight.

-   :material-memory:{ .lg .middle } **Mémoire bornée**

    ---

    Les documents inactifs sont compactés en JSON texte (environ 10 fois plus
    petit), et les documents propres inactifs quittent la mémoire. La RAM suit
    les données actives, pas tout ce qui a été lu depuis le démarrage.

-   :material-lock-check-outline:{ .lg .middle } **Aucune mise à jour perdue**

    ---

    Les opérations lecture-modification-écriture sont sérialisées par clé.
    Seize workers sur un même compteur tombent pile sur la bonne somme, vérifié
    à chaque passe de benchmark.

-   :material-file-tree-outline:{ .lg .middle } **Clés imbriquées**

    ---

    `user.profile.name` adresse l'intérieur d'un document par clé de base,
    jusqu'à 32 niveaux. Une lecture ne copie que le sous-arbre demandé.

-   :material-reply-outline:{ .lg .middle } **Des réponses, pas des accusés**

    ---

    `add` / `sub` renvoient le nouveau nombre, `delete` si la clé existait,
    `push` / `pull` le tableau obtenu sur demande.

-   :material-shield-check-outline:{ .lg .middle } **Durci par défaut**

    ---

    Authentification argon2id ou à temps constant, noms de tables validés,
    messages bornés, contre-pression par connexion, échéances, pings des liens
    inactifs.

</div>

## Pour continuer

<div class="grid cards" markdown>

-   :material-rocket-launch-outline:{ .lg .middle } [**Démarrage**](getting-started.md)

    Un serveur et un conteneur PostgreSQL en un script, puis votre première
    requête.

-   :material-tune-variant:{ .lg .middle } [**Configuration**](configuration.md)

    Chaque option de `[server]`, `[database]` et `[auth]`, sa valeur par défaut
    et sa règle de validation.

-   :material-sitemap-outline:{ .lg .middle } [**Architecture**](architecture.md)

    Write-behind, générations des marques sales, flush par lots, chargements à
    froid et les trois niveaux de mémoire.

-   :material-swap-horizontal:{ .lg .middle } [**Protocole**](protocol.md)

    Format des paquets, chaque opération avec requête et réponse, erreurs et
    limites.

-   :material-language-typescript:{ .lg .middle } [**SDK TypeScript**](sdk.md)

    Le client Bun : options, méthodes, classes d'erreur, reconnexion.

-   :material-server-outline:{ .lg .middle } [**Exploitation**](operations.md)

    systemd et pm2, arrêt propre, lignes de stats, réglage de la mémoire,
    sauvegardes.

-   :material-speedometer:{ .lg .middle } [**Benchmarks**](benchmarks.md)

    Méthode, chiffres de référence, mesures mémoire et
    [résultats interactifs](https://ihrz.gitlab.io/db/benchmarks/).

-   :material-help-circle-outline:{ .lg .middle } [**FAQ**](faq.md)

    Redémarrages, démarrage à froid, consommation mémoire, connexions fermées
    et autres surprises.

</div>

!!! note "État du projet"

    HorizonDB fait partie du projet **iHorizon**. C'est un travail en cours : le
    protocole peut encore changer, mettez donc à jour le serveur et le SDK
    ensemble.

[:material-weather-sunset: Page d'accueil](https://ihrz.gitlab.io/db/){ .md-button }
[:material-palette-outline: Kit de marque](https://ihrz.gitlab.io/db/brand/){ .md-button }
[:fontawesome-brands-gitlab: Code source](https://gitlab.com/ihrz/db){ .md-button }
