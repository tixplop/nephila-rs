package org.exemple.mediatheque.notification

import org.exemple.mediatheque.model.Adherent

/** Envoie des messages aux adherents. */
interface Notificateur {
    fun prevenir(adherent: Adherent, message: String)
}

class NotificateurConsole : Notificateur {
    override fun prevenir(adherent: Adherent, message: String) {
        println("Carte ${adherent.numeroCarte} : $message")
    }
}
