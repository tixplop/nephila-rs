// Reservation d'un livre deja prete.
import { ReservationDto } from './types';

export class GestionReservation {
  private attente: ReservationDto[] = [];

  reserver(isbn: string, carte: number): ReservationDto {
    const rang = this.attente.filter((r) => r.isbn === isbn).length + 1;
    const reservation = { isbn, carte, rang };
    this.attente.push(reservation);
    return reservation;
  }

  annuler(isbn: string, carte: number): void {
    this.attente = this.attente.filter((r) => !(r.isbn === isbn && r.carte === carte));
  }
}
