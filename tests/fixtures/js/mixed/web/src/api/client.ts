import { Id, toId } from './types';

export class ApiClient {
  fetchOrder(raw: string): Id {
    return toId(raw);
  }
}
