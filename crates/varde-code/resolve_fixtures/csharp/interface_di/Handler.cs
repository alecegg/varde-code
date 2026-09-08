public class Handler
{
    private readonly IUserService _svc;
    public int Run() { return _svc.ListUsers(); }
}
