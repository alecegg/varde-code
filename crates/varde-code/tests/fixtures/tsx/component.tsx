// Realistic TSX: a React component tree with JSX, hooks, handlers, and
// error handling. Covers the structural, statement, expression, and
// control-flow/error kinds of the 14-kind checklist. JSX element nodes
// (jsx_element / jsx_self_closing_element) must produce no entity kinds.

import { Component, useEffect, useState, type FormEvent } from "react";

export interface TodoItem {
  id: number;
  title: string;
  done: boolean;
}

interface AppProps {
  items: TodoItem[];
  onToggle: (id: number) => void;
}

const TITLE = "Todo List";
const empty = null;

function Header({ title }: { title: string }): JSX.Element {
  return <h1 className="app-header">{title}</h1>;
}

class TaskRow extends Component<{ item: TodoItem; onToggle: (id: number) => void }> {
  render(): JSX.Element {
    const item = this.props.item;
    return (
      <li className={item.done ? "done" : "todo"}>
        <span>{item.title}</span>
        <button type="button" onClick={() => this.handleToggle(item.id)}>
          Toggle
        </button>
      </li>
    );
  }

  handleToggle(id: number): void {
    this.props.onToggle(id);
  }
}

export function TodoApp(props: AppProps): JSX.Element {
  const [items, setItems] = useState<TodoItem[]>(props.items);
  const [filter, setFilter] = useState<string>("all");

  useEffect(() => {
    document.title = TITLE;
  }, []);

  function toggle(id: number): void {
    setItems(
      items.map((item) =>
        item.id === id ? { ...item, done: !item.done } : item
      )
    );
    try {
      persist(items);
    } catch (err) {
      console.error(err);
    }
  }

  function save(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    if (items.length === 0) {
      throw new Error("cannot save empty list");
    }
    const form = event.currentTarget;
    const title = form.title.value;
    for (let i = 0; i < items.length; i++) {
      if (items[i].title === title) {
        return;
      }
    }
    setItems([...items, { id: Date.now(), title, done: false }]);
  }

  return (
    <div className="todo-app">
      <Header title={TITLE} />
      <ul>
        {items.map((item) => (
          <li key={item.id}>
            <input
              type="checkbox"
              checked={item.done}
              onChange={() => toggle(item.id)}
            />
            <span>{item.title}</span>
          </li>
        ))}
      </ul>
      <form onSubmit={save}>
        <input name="title" placeholder="Add a task" />
        <button type="submit">Add</button>
      </form>
    </div>
  );
}

export default TodoApp;
