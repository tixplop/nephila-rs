# Architecture

Le code est organise en trois couches, de la plus proche de la base a la plus
proche de l'utilisateur.

## Depots

Les interfaces `LivreRepository`, `AdherentRepository`, `EmpruntRepository` et
`AmendeRepository` isolent l'acces aux tables du schema decrit dans
`db/schema`.

## Services

`EmpruntService` enregistre les prets et les retours. Au retour, il confie le
calcul de l'amende a `AmendeService`. `CatalogueService` gere la recherche des
livres et `ReservationService` la file d'attente.

## Notifications et client

Le planificateur de rappels previent les adherents en retard. Le client web
appelle le serveur par `api.js` ; voir le [guide d'emprunt](guide-emprunt.md).
