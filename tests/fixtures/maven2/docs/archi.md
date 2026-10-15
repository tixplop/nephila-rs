# Architecture

Le service `OrderService` valide la commande puis la confie à `OrderRepository`.
Voir aussi le [guide](guide.md).
Retour à l'[accueil](../README.md#install), voir le [site](https://x.org/a.md) et la page [absente](absent.md).

## Détails

La méthode `OrderRepository.save()` (aussi notée `OrderRepository#save`) persiste la commande.
