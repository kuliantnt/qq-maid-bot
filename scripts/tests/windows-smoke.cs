using System;
using System.Threading;

public static class WindowsSmoke
{
    public static void Main()
    {
        Console.WriteLine("windows smoke started");
        Console.Out.Flush();
        while (true)
        {
            Thread.Sleep(250);
        }
    }
}
