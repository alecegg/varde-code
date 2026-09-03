package com.example.app;

import java.util.List;

/**
 * Public top-level class -> Class + Export.
 */
public class TodoController {
    private final List<String> items = new ArrayList<>();
    public static final int MAX_ITEMS = 100;

    public TodoController() {
        this.items = new ArrayList<>();
    }

    @GetMapping("/todos")
    public List<String> list(String filter) {
        int count = 0;
        if (filter != null) {
            count = items.size();
        }
        String first = this.items.get(0);
        return items;
    }

    @PostMapping("/todos")
    public void create(String title) {
        try {
            items.add(title);
        } catch (IllegalArgumentException e) {
            throw new RuntimeException("invalid title", e);
        }
    }

    public void delete(int id, HttpServletResponse response) {
        boolean found = false;
        for (int i = 0; i < items.size(); i++) {
            if (i == id) {
                found = true;
                break;
            }
        }
        for (String item : items) {
            if (item.isEmpty()) {
                continue;
            }
        }
        while (found) {
            items.remove(0);
            found = false;
        }
        do {
            items.remove(0);
        } while (!items.isEmpty());
        switch (items.size()) {
            case 0:
                break;
            default:
                items.clear();
        }
        String msg = found ? "yes" : "no";
        int n = 42;
        double d = 3.14;
        char c = 'x';
        String s = null;
        if (s == null) {
            response.setStatus(200);
            return;
        }
        response.setStatus(404);
    }
}

/**
 * Public top-level interface -> Interface + Export.
 */
public interface Repository {
    String findById(long id);
}
