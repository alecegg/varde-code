namespace TodoApp;

public static class Flow
{
    public static int Count(int[] numbers, int limit)
    {
        int total = 0;
        bool done = false;
        char sep = ';';
        double ratio = 3.14;
        string label = null;
        for (int i = 0; i < numbers.Length; i++)
        {
            if (numbers[i] > limit)
            {
                total++;
                continue;
            }
            if (numbers[i] == 0)
            {
                break;
            }
        }
        foreach (int n in numbers)
        {
            total += n;
        }
        int j = 0;
        while (j < 10)
        {
            j++;
        }
        do
        {
            j--;
        } while (j > 0);
        switch (total)
        {
            case 0:
                return 0;
            default:
                break;
        }
        label = total > 0 ? "positive" : "zero";
        return total;
    }
}
