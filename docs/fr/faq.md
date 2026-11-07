---
title: FAQ et dépannage
description: Les réponses aux questions et surprises qui reviennent quand on fait tourner HorizonDB.
---

# FAQ et dépannage

## Démarrage et redémarrages

??? question "Le redémarrage logue `port in use, retrying…` ou échoue sur `EADDRINUSE`"

    Au démarrage, le serveur réessaie un port occupé toutes les 500 ms pendant
    **15 s**. Cela couvre un redémarrage où l'instance précédente termine son
    flush final : l'ancien processus ferme son écoute d'abord, puis flushe
    pendant 10 s au plus, et le nouveau récupère le port dans la fenêtre.

    Si l'échec persiste après 15 s (`port still in use after retry window, giving up`,
    puis `failed to bind`), quelque chose d'autre tient le port. Trouvez-le
    avec `ss -ltnp 'sport = :8080'` : souvent une seconde instance lancée à la
    main, ou deux superviseurs (systemd **et** pm2) qui gèrent le même binaire.

??? question "Le serveur s'arrête juste après avoir démarré"

    Regardez la dernière ligne de log :

    - `loading configuration file … failed` : une erreur de validation,
      nommée dans le message (voir [Configuration](configuration.md)).
    - `HorizonDB initialization failed: database connection failed` :
      PostgreSQL est injoignable ou a refusé les identifiants. Le serveur se
      connecte et exécute `SELECT 1` avant d'écouter.
    - Avec `scripts/dev-up.sh`, vérifiez le mot de passe du `config.toml`
      généré (voir [Démarrage](getting-started.md#dev-script)).

??? question "Après un redémarrage, chaque requête répond `invalid or expired session`"

    Les sessions vivent en mémoire et ne survivent pas à un redémarrage.
    Reconnectez-vous. Le SDK le fait automatiquement, une fois, quand il voit
    une erreur de session.

## Performances

??? question "Les premières requêtes après un redémarrage sont lentes"

    C'est le démarrage à froid : la mémoire part vide et chaque clé est chargée
    depuis PostgreSQL à son premier accès (un `SELECT` par clé de base, puis un
    parsing, sur un thread bloquant pour les documents de 256 Kio et plus). Les
    accès simultanés à la même clé absente partagent un seul chargement. Pour
    raccourcir cette phase :

    - donnez au pool assez de connexions (`database.max_connections`) pour le
      nombre de clients qui lisent en même temps ;
    - gardez des documents de base de taille raisonnable : le document entier
      est chargé, même pour un `get` imbriqué.

    Le même coût s'applique à un document rechargé après une
    [éviction](architecture.md#memory-tiers) ; `memory_hit_rate` vous dit à
    quelle fréquence cela arrive.

??? question "Des requêtes répondent `deadline exceeded`"

    La requête a attendu plus que son `timeoutMs` (10 s par défaut) avant que
    le serveur puisse la démarrer, en général coincée derrière 1 024 autres
    requêtes sur la même connexion. Le serveur l'a sautée parce que le client
    avait déjà abandonné. Répartissez la charge sur plus de connexions,
    réduisez la profondeur du pipeline, ou cherchez les opérations lentes
    (parcours `all`, chargements à froid) dans les logs de debug.

??? question "Seize workers sur un même compteur vont moins vite que sur des clés différentes"

    C'est voulu : les opérations lecture-modification-écriture sur une même clé
    sont sérialisées pour ne perdre aucune mise à jour. Des clés différentes
    avancent en parallèle (sauf si elles tombent sur le même des 256 mutex).
    Répartissez les compteurs très sollicités sur plusieurs clés si la
    contention compte.

## Mémoire

??? question "Pourquoi le RSS est-il bien plus gros que mes données ?"

    Plusieurs raisons, de la plus importante à la moins importante :

    1. **Un arbre JSON parsé est bien plus gros que son texte**, environ dix
       fois (~11,2 Ko en mémoire pour un document de ~940 octets). Compacter
       les documents inactifs (`pack_idle_secs`) les ramène près de leur
       taille JSON.
    2. **Tout ce qui est lu est gardé** jusqu'à ce que le document reste
       inactif `idle_eviction_secs` ; avec `0`, jusqu'au redémarrage.
    3. **Les documents sales ne peuvent pas partir** avant le prochain flush, et
       les clés TTL jamais avant leur expiration.
    4. **L'allocateur rend la mémoire progressivement.** jemalloc rend les pages
       libérées au système selon une temporisation : le RSS baisse un peu après
       le balayage.

    Comparez `allocated` et `resident` sur la ligne
    [`memory stats`](operations.md#stats), et voyez le
    [guide de réglage](operations.md#memory-tuning).

??? question "`memory_entries` ne bouge pas juste après une rafale de trafic"

    `memory_entries` et `memory_packed` sont mis à jour par le balayage, une
    fois par minute. `memory_evicted` est un cumul depuis le démarrage.

## Connexions et messages

??? question "Ma connexion se ferme quand j'écris un gros document"

    Le message dépassait `server.max_message_size` (64 Mio par défaut). Le
    serveur ferme la connexion entière, et toutes les requêtes en vol dessus
    échouent (le SDK lève `ConnectionClosedError` et se reconnecte). Le log du
    serveur affiche `connection ended with error` avec une erreur de capacité.
    Relevez la limite au-dessus de votre plus gros document (256 Mio au plus),
    ou répartissez le document sur plusieurs clés de base.

??? question "Des connexions sont coupées au bout de deux minutes"

    Le serveur envoie un ping après 30 s sans trafic et coupe après 120 s. Un
    client WebSocket standard répond aux pings tout seul : seuls les clients
    bloqués, ou placés derrière un proxy qui avale les trames de contrôle, sont
    touchés.

??? question "De nouvelles connexions sont refusées sous charge"

    `server.max_connections` (1 024 par défaut) est atteint ; le log indique
    `connection limit reached, rejecting`. Partagez les connexions (une
    instance du SDK et ses vues `table()` utilisent un seul socket) ou relevez
    la limite.

## Données

??? question "J'ai modifié une ligne dans PostgreSQL et le serveur l'ignore"

    Les documents déjà en mémoire sont servis depuis la mémoire, et un
    document sale écrase la ligne au prochain flush. Modifiez les données via
    le serveur, ou arrêtez-le, modifiez les tables, et redémarrez-le.

??? question "J'ai perdu des écritures après un crash"

    Les écritures restent en mémoire jusqu'au prochain flush, toutes les
    `flush_interval_secs` (300 s par défaut). Un crash ou un `kill -9` perd ce
    qui n'a pas encore été flushé ; SIGINT et SIGTERM flushent d'abord.
    Baissez l'intervalle si la fenêtre est trop large. Voir
    [durabilité](architecture.md#durability).

??? question "`add` renvoie `5.0` au lieu de `5`"

    Les nombres modifiés par `add` et `sub` sont des flottants 64 bits, écrits
    comme tels en JSON. JavaScript lit les deux de la même façon. Les entiers
    au-delà de 2^53 perdent en précision : n'utilisez pas `add` sur des ids de
    la taille d'un snowflake.

??? question "Une clé écrite avec `cache` est revenue après un redémarrage"

    L'échéance d'un TTL n'existe qu'en mémoire. Si un flush a écrit la clé dans
    PostgreSQL et que le serveur a redémarré avant son expiration, elle est
    rechargée comme une clé permanente. Supprimez-la explicitement si c'est
    important.

??? question "`all` ne renvoie pas toutes les lignes"

    Il s'arrête à 10 000 lignes, triées par id. Il est fait pour les petites
    tables, pas comme API de pagination.

??? question "`get` renvoie `undefined` pour une valeur stockée à `null`"

    Le SDK transforme une réponse `null` sans valeur par défaut en
    `undefined`, et le `has` du serveur traite un `null` stocké comme absent.
    Stockez une autre valeur sentinelle si vous devez les distinguer.

## Authentification

??? question "Chaque login répond `invalid credentials` avec un `password_hash`"

    Le hash n'est parsé qu'au moment du login. Une chaîne tronquée, mal
    recopiée ou qui n'est pas au format PHC fait échouer chaque vérification
    sans aucune erreur au démarrage. Collez la chaîne PHC complète
    (`$argon2id$v=19$m=…,t=…,p=…$sel$hash`) entre apostrophes, voir
    [Générer un password_hash](configuration.md#password-hash).

??? question "Y a-t-il du TLS ?"

    Pas dans le serveur : il parle `ws://` en clair. Gardez-le sur un réseau
    privé, ou terminez le TLS dans un reverse proxy placé devant et
    connectez-vous en `wss://`.
