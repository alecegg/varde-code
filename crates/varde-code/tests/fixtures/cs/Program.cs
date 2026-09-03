using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using System.Collections.Generic;

namespace TodoApp;

public class Program
{
    private readonly List<string> items = new List<string>();
    public const int MaxItems = 100;

    public Program()
    {
        items = new List<string>();
    }

    public static void Main(string[] args)
    {
        var builder = WebApplication.CreateBuilder(args);
        var app = builder.Build();
        app.MapGet("/todos", () => GetTodos());
        app.MapPost("/todos", (Todo todo) => CreateTodo(todo));
        app.MapDelete("/todos/{id}", (int id) => DeleteTodo(id));
        app.Run();
    }

    public static string GetTodos()
    {
        return "[]";
    }

    public static string CreateTodo(Todo todo)
    {
        try
        {
            if (todo == null)
            {
                throw new System.ArgumentException("null todo");
            }
            return Results.Ok(todo).ToString();
        }
        catch (System.ArgumentException e)
        {
            return "error";
        }
    }

    public static string DeleteTodo(int id)
    {
        var result = Results.Json(id);
        return result.ToString();
    }
}

public interface ITodoStore
{
    string FindById(long id);
}

public class Todo
{
    public string Title { get; set; }
}
