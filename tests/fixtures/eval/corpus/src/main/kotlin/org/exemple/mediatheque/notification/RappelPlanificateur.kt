package org.exemple.mediatheque.notification

import java.time.LocalDate
import org.exemple.mediatheque.repository.EmpruntRepository

/** Previent chaque jour les adherents dont un livre est en retard. */
class RappelPlanificateur(
    private val emprunts: EmpruntRepository,
    private val notificateur: Notificateur
) {
    fun envoyerRappels(jour: LocalDate): Int {
        val enRetard = emprunts.empruntsEnRetard(jour)
        for (emprunt in enRetard) {
            notificateur.prevenir(emprunt.adherent, "Merci de rendre ${emprunt.livre.titre}")
        }
        return enRetard.size
    }
}
