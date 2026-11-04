package org.exemple.mediatheque.service;

import java.util.ArrayDeque;
import java.util.HashMap;
import java.util.Map;
import java.util.Queue;
import org.exemple.mediatheque.model.Adherent;

/** File d'attente des reservations pour les livres deja prets. */
public class ReservationService {
    private final Map<String, Queue<Adherent>> files = new HashMap<>();

    public int reserver(String isbn, Adherent adherent) {
        Queue<Adherent> file = files.computeIfAbsent(isbn, k -> new ArrayDeque<>());
        file.add(adherent);
        return file.size();
    }

    public Adherent prochainAdherent(String isbn) {
        Queue<Adherent> file = files.get(isbn);
        return file == null ? null : file.poll();
    }
}
