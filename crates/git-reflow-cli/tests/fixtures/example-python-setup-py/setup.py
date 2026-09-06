#!/usr/bin/env python
from setuptools import find_packages, setup

setup(
    name="example-python-setup-py",
    version="1.0.0",
    description="An example project.",
    long_description=open("README.md", encoding="utf-8").read(),
    long_description_content_type="text/markdown",
    author="Jonathan Hiles",
    author_email="jonathan@hil.es",
    packages=find_packages(include=["example", "example.*"]),
    python_requires=">=3.14",
)
