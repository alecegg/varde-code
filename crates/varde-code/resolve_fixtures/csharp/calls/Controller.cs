public class WidgetController : ControllerBase
{
    private readonly WidgetService _svc;

    public int Handle() { return _svc.DoUnique(); }
    public int Handle2() { return _svc.DoAmbiguous(); }
    public WidgetService Build() { return new WidgetService(); }
}
