import { Item } from './Item';
type Props = { items: string[] };
export const List = ({ items }: Props) => <ul>{items.map(i => <Item key={i}/>)}</ul>;
