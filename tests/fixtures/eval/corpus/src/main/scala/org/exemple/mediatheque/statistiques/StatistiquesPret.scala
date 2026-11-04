package org.exemple.mediatheque.statistiques

import org.exemple.mediatheque.model.Emprunt

/** Statistiques sur les prets : livres les plus empruntes. */
object StatistiquesPret {
  def livresPopulaires(emprunts: Seq[Emprunt], n: Int): Seq[String] =
    emprunts
      .groupBy(_.getLivre.getIsbn)
      .toSeq
      .sortBy { case (_, liste) => -liste.size }
      .take(n)
      .map(_._1)

  def moyenneRetard(emprunts: Seq[Emprunt]): Double = {
    if (emprunts.isEmpty) 0.0
    else emprunts.map(_.joursDeRetard(java.time.LocalDate.now)).sum.toDouble / emprunts.size
  }
}
