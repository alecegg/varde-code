namespace App;

public class OrderService
{
    // `IWidgetRepository` is not defined anywhere in the repo: it is a
    // library/framework type. The receiver is therefore positively external.
    private readonly IWidgetRepository _repo;

    public OrderService(IWidgetRepository repo)
    {
        _repo = repo;
    }

    public void Run()
    {
        // Library-receiver call: must NOT bind to Catalog.FindOne via the
        // repo-wide single-definition fallback (the veto drops the false edge).
        _repo.FindOne();
    }
}
