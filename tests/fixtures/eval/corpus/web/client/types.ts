// Types partages du client web.
export interface LivreDto {
  isbn: string;
  titre: string;
  auteur: string;
  disponible: boolean;
}

export interface ReservationDto {
  isbn: string;
  carte: number;
  rang: number;
}
