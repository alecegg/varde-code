public class ShipHandler
{
    private readonly IShipping _ship;
    public int Run() { return _ship.Process(); }
}
